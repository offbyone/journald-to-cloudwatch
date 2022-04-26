use crate::configuration::Configuration;
use async_trait::async_trait;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_cloudwatchlogs::model::{InputLogEvent, LogStream};
use aws_sdk_cloudwatchlogs::{Client, Region};
use chrono::Utc;
use std::time::Duration;

use tokio::sync::mpsc;

#[async_trait]
trait Uploader {
    fn group_events(
        &self,
        events: Vec<InputLogEvent>,
    ) -> Vec<Vec<InputLogEvent>>;
    async fn upload(&mut self, events: Vec<InputLogEvent>);
}

struct CloudWatch {
    client: Client,
    sequence_token: Option<String>,
    conf: Configuration,
}

impl CloudWatch {
    async fn new(conf: Configuration) -> CloudWatch {
        let region_provider = RegionProviderChain::default_provider()
            .or_else(Region::new("us-west-2"));

        let shared_config =
            aws_config::from_env().region(region_provider).load().await;

        let client = Client::new(&shared_config);

        let mut cw = CloudWatch {
            sequence_token: None,
            client,
            conf,
        };
        cw.update_sequence_token().await;
        cw
    }

    async fn get_log_stream(&self) -> Option<LogStream> {
        let result = self
            .client
            .describe_log_streams()
            .log_group_name(self.conf.log_group_name.clone())
            .log_stream_name_prefix(self.conf.log_stream_name.clone())
            .limit(1)
            .send()
            .await;
        match result {
            Ok(result) => {
                if let Some(log_streams) = result.log_streams {
                    if let Some(log_stream) = log_streams.first() {
                        if log_stream.log_stream_name
                            == Some(self.conf.log_stream_name.clone())
                        {
                            return Some(log_stream.clone());
                        }
                    }
                }
                None
            }
            Err(_) => None,
        }
    }

    async fn create_log_stream(&self) {
        if let Err(err) = self
            .client
            .create_log_stream()
            .log_group_name(self.conf.log_group_name.clone())
            .log_stream_name(self.conf.log_stream_name.clone())
            .send()
            .await
        {
            eprintln!("failed to create log stream: {}", err);
        }
    }

    async fn update_sequence_token(&mut self) {
        let mut log_stream = self.get_log_stream().await;
        if log_stream.is_none() {
            self.create_log_stream().await;
            log_stream = self.get_log_stream().await;
        }

        if let Some(log_stream) = log_stream {
            self.sequence_token = log_stream.upload_sequence_token;
        } else {
            eprintln!("log stream {} does not exist", self.conf.path());
        }
    }
}

fn do_group_events(events: Vec<InputLogEvent>) -> Vec<Vec<InputLogEvent>> {
    // Group events by 16 hour windows (cloudwatch requires events be in 24 groups)

    // Why in this form? Because it's a bit easier to understand that it's a time.
    let sixteen =
        i64::try_from(Duration::from_secs(57600).as_millis()).unwrap();

    let mut groups: Vec<Vec<InputLogEvent>> = Vec::new();
    // First, we order the events by their timestamps
    let mut sorted = events.to_vec();
    sorted.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    for event in sorted.into_iter() {
        if let None = groups.last() {
            let mut new_group = Vec::new();
            new_group.push(event);
            groups.push(new_group);
            continue;
        }

        // check to see if the last group is the one we want
        let mut existing_group = groups.pop().unwrap();
        let first = existing_group.first().unwrap();
        let too_new = match event.timestamp {
            Some(ts) => match first.timestamp {
                Some(fts) => ts - sixteen > fts,
                None => true,
            },
            None => true,
        };

        if too_new {
            // too new; make a new group
            // but first, put the old one back
            groups.push(existing_group);
            let mut new_group = Vec::new();
            new_group.push(event);
            groups.push(new_group);
        } else {
            existing_group.push(event);
            groups.push(existing_group);
        }
    }
    groups
}

#[async_trait]
impl Uploader for CloudWatch {
    fn group_events(
        &self,
        events: Vec<InputLogEvent>,
    ) -> Vec<Vec<InputLogEvent>> {
        do_group_events(events)
    }

    async fn upload(&mut self, events: Vec<InputLogEvent>) {
        self.conf
            .debug(format!("--F> uploading {} events", events.len()));
        for group in self.group_events(events).iter() {
            let mut call = self
                .client
                .put_log_events()
                .log_group_name(self.conf.log_group_name.clone())
                .log_stream_name(self.conf.log_stream_name.clone());
            if let Some(sequence_token) = &self.sequence_token {
                call = call.sequence_token(sequence_token);
            }
            call = call.set_log_events(Some(group.to_vec()));
            let result = call.send().await;
            match result {
                Ok(result) => {
                    self.sequence_token = result.next_sequence_token;
                }
                Err(err) => {
                    eprintln!("--F> send_to_cloudwatch failed: {}", err);
                    self.update_sequence_token().await
                }
            }
        }
    }
}

/// Calculate the number of bytes this message requires as counted
/// by the PutLogEvents API.
///
/// Reference:
/// docs.aws.amazon.com/AmazonCloudWatchLogs/latest/APIReference/API_PutLogEvents.html
fn get_event_num_bytes(event: &InputLogEvent) -> usize {
    match &event.message {
        Some(m) => m.len() + 26,
        None => 26,
    }
}

struct UploadThreadState<U: Uploader> {
    conf: Configuration,
    uploader: U,
    events: Vec<InputLogEvent>,
    first_timestamp: Option<i64>,
    last_timestamp: Option<i64>,
    num_pending_bytes: usize,
}

impl<U: Uploader> UploadThreadState<U> {
    fn new(uploader: U, conf: Configuration) -> UploadThreadState<U> {
        UploadThreadState {
            conf,
            uploader,
            events: Vec::new(),
            first_timestamp: None,
            last_timestamp: None,
            num_pending_bytes: 0,
        }
    }

    async fn push(&mut self, event: InputLogEvent) {
        // Flush if the latest event's timestamp is older than the
        // previous event
        if let Some(last_timestamp) = self.last_timestamp {
            if event.timestamp < Some(last_timestamp) {
                self.flush().await;
            }
        }

        // Flush if the maximum size (in bytes) of events has been reached
        let max_bytes = 1048576;
        let event_num_bytes = get_event_num_bytes(&event);
        if self.num_pending_bytes + event_num_bytes > max_bytes {
            self.flush().await;
        }

        // Flush if the maximum number of events has been reached
        let max_events = if self.conf.is_debug_mode_enabled {
            1
        } else {
            100
        };
        if self.events.len() + 1 >= max_events {
            self.flush().await;
        }

        // Add the event to the pending events
        if self.first_timestamp.is_none() {
            self.first_timestamp = event.timestamp;
        }
        self.last_timestamp = event.timestamp;
        self.num_pending_bytes += event_num_bytes;
        self.events.push(event);
    }

    /// Upload all pending events to CloudWatch Logs
    async fn flush(&mut self) {
        self.conf.debug(format!("flush: {}", self.summary()));

        if self.events.is_empty() {
            return;
        }

        let mut events = Vec::new();
        std::mem::swap(&mut events, &mut self.events);
        self.uploader.upload(events).await;
        self.first_timestamp = None;
        self.last_timestamp = None;
        self.num_pending_bytes = 0;
    }

    fn summary(&self) -> String {
        format!("events.len()={}, first_timestamp={:?}, last_timestamp={:?}, num_pending_bytes={}",
                self.events.len(),
                self.first_timestamp,
                self.last_timestamp, self.num_pending_bytes)
    }
}

pub async fn upload_thread(
    conf: Configuration,
    mut rx: mpsc::Receiver<InputLogEvent>,
) {
    conf.debug("upload thread started".to_string());
    let uploader = CloudWatch::new(conf.clone()).await;
    let mut state = UploadThreadState::new(uploader, conf.clone());
    while let Some(record) = rx.recv().await {
        state.push(record).await;
    }
    conf.debug(
        "The receiver has been dropped and the event queue is drained"
            .to_string(),
    );

    // If we have "old" records, flush now
    if let Some(first_timestamp) = state.first_timestamp {
        if Utc::now().timestamp_millis() - first_timestamp > 1000 {
            state.flush().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use aws_types::SdkConfig;

    use super::*;

    fn create_conf() -> Configuration {
        Configuration {
            log_group_name: "myGroup".to_string(),
            log_stream_name: "myStream".to_string(),
            is_debug_mode_enabled: false,
            aws_config: SdkConfig::builder()
                .region(Region::from_static("us-test-2"))
                .build(),
        }
    }

    struct MockUploader {
        events: Vec<InputLogEvent>,
    }

    impl MockUploader {
        fn new() -> MockUploader {
            MockUploader { events: Vec::new() }
        }
    }

    #[async_trait]
    impl Uploader for MockUploader {
        fn group_events(
            &self,
            events: Vec<InputLogEvent>,
        ) -> Vec<Vec<InputLogEvent>> {
            super::do_group_events(events)
        }
        async fn upload(&mut self, mut events: Vec<InputLogEvent>) {
            self.events.append(&mut events);
        }
    }

    #[tokio::test]
    async fn test_manual_flush() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state
            .push(
                InputLogEvent::builder()
                    .message("myMessage".to_string())
                    .timestamp(Utc::now().timestamp_millis())
                    .build(),
            )
            .await;
        assert_eq!(state.uploader.events.len(), 0);
        state.flush().await;
        assert_eq!(state.uploader.events.len(), 1);
    }

    #[tokio::test]
    async fn test_out_of_order_events() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state
            .push(
                InputLogEvent::builder()
                    .message("myMessage1".to_string())
                    .timestamp(2)
                    .build(),
            )
            .await;
        assert_eq!(state.uploader.events.len(), 0);
        state
            .push(
                InputLogEvent::builder()
                    .message("myMessage2".to_string())
                    .timestamp(1)
                    .build(),
            )
            .await;
        assert_eq!(state.uploader.events.len(), 1);
    }

    #[tokio::test]
    async fn test_simultaneous_events() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state
            .push(
                InputLogEvent::builder()
                    .message("myMessage1".to_string())
                    .timestamp(1)
                    .build(),
            )
            .await;
        assert_eq!(state.uploader.events.len(), 0);
        state
            .push(
                InputLogEvent::builder()
                    .message("myMessage2".to_string())
                    .timestamp(1)
                    .build(),
            )
            .await;
        assert_eq!(state.uploader.events.len(), 0);
    }

    #[test]
    fn test_events_more_than_24h_apart() {
        let uploader = MockUploader::new();
        let sooner = Utc::now().timestamp_millis()
            - i64::try_from(Duration::from_secs(86400 * 2).as_millis())
                .unwrap();
        let later = Utc::now().timestamp_millis();
        let mut events = Vec::with_capacity(3);
        events.push(
            InputLogEvent::builder()
                .message("ev1".to_string())
                .timestamp(sooner)
                .build(),
        );
        events.push(
            InputLogEvent::builder()
                .message("ev2".to_string())
                .timestamp(sooner + 42)
                .build(),
        );
        events.push(
            InputLogEvent::builder()
                .message("ev3".to_string())
                .timestamp(later)
                .build(),
        );
        assert_eq!(uploader.group_events(events).len(), 2);
    }

    #[test]
    fn test_events_separated_by_17_apart() {
        let uploader = MockUploader::new();
        let interval =
            i64::try_from(Duration::from_secs(17 * 60 * 60).as_millis())
                .unwrap();
        let mut events = Vec::with_capacity(3);
        let now = Utc::now().timestamp_millis();
        events.push(
            InputLogEvent::builder()
                .message("ev1".to_string())
                .timestamp(now - (2 * interval))
                .build(),
        );
        events.push(
            InputLogEvent::builder()
                .message("ev2".to_string())
                .timestamp(now - interval)
                .build(),
        );
        events.push(
            InputLogEvent::builder()
                .message("ev3".to_string())
                .timestamp(now)
                .build(),
        );
        assert_eq!(uploader.group_events(events).len(), 3);
    }
}
