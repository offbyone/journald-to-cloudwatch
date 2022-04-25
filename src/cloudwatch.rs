use crate::configuration::Configuration;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_cloudwatchlogs::model::{InputLogEvent, LogStream};
use aws_sdk_cloudwatchlogs::{Client, Region};
use chrono::Utc;

use tokio::sync::mpsc;

trait Uploader {
    fn upload(&mut self, events: Vec<InputLogEvent>);
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

impl Uploader for CloudWatch {
    fn upload(&mut self, events: Vec<InputLogEvent>) {
        self.conf
            .debug(format!("uploading {} events", events.len()));
        let mut call = self
            .client
            .put_log_events()
            .log_group_name(self.conf.log_group_name.clone())
            .log_stream_name(self.conf.log_stream_name.clone());
        if let Some(sequence_token) = &self.sequence_token {
            call = call.sequence_token(sequence_token);
        }
        call = call.set_log_events(Some(events));
        let result = futures::executor::block_on(call.send());
        match result {
            Ok(result) => {
                self.sequence_token = result.next_sequence_token;
            }
            Err(err) => {
                eprintln!("send_to_cloudwatch failed: {}", err);
                futures::executor::block_on(self.update_sequence_token());
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

    fn push(&mut self, event: InputLogEvent) {
        self.conf.debug("upload thread event received".to_string());

        // Flush if the latest event's timestamp is older than the
        // previous event
        if let Some(last_timestamp) = self.last_timestamp {
            if event.timestamp < Some(last_timestamp) {
                self.flush();
            }
        }

        // Flush if the maximum size (in bytes) of events has been reached
        let max_bytes = 1048576;
        let event_num_bytes = get_event_num_bytes(&event);
        if self.num_pending_bytes + event_num_bytes > max_bytes {
            self.flush();
        }

        // Flush if the maximum number of events has been reached
        let max_events = 10000;
        if self.events.len() + 1 >= max_events {
            self.flush();
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
    fn flush(&mut self) {
        self.conf.debug(format!("flush: {}", self.summary()));

        if self.events.is_empty() {
            return;
        }

        let mut events = Vec::new();
        std::mem::swap(&mut events, &mut self.events);
        self.uploader.upload(events);
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
    mut rx: mpsc::UnboundedReceiver<InputLogEvent>,
) {
    conf.debug("upload thread started".to_string());
    let uploader = CloudWatch::new(conf.clone()).await;
    let mut state = UploadThreadState::new(uploader, conf.clone());
    loop {
        conf.debug(format!("upload thread state: {}", state.summary()));

        if let Some(record) = rx.recv().await {
            state.push(record);
        }

        // If we have "old" records, flush now
        if let Some(first_timestamp) = state.first_timestamp {
            if Utc::now().timestamp_millis() - first_timestamp > 1000 {
                state.flush();
            }
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

    impl Uploader for MockUploader {
        fn upload(&mut self, mut events: Vec<InputLogEvent>) {
            self.events.append(&mut events);
        }
    }

    #[test]
    fn test_manual_flush() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state.push(
            InputLogEvent::builder()
                .message("myMessage".to_string())
                .timestamp(Utc::now().timestamp_millis())
                .build(),
        );
        assert_eq!(state.uploader.events.len(), 0);
        state.flush();
        assert_eq!(state.uploader.events.len(), 1);
    }

    #[test]
    fn test_out_of_order_events() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state.push(
            InputLogEvent::builder()
                .message("myMessage1".to_string())
                .timestamp(2)
                .build(),
        );
        assert_eq!(state.uploader.events.len(), 0);
        state.push(
            InputLogEvent::builder()
                .message("myMessage2".to_string())
                .timestamp(1)
                .build(),
        );
        assert_eq!(state.uploader.events.len(), 1);
    }

    #[test]
    fn test_simultaneous_events() {
        let uploader = MockUploader::new();
        let mut state = UploadThreadState::new(uploader, create_conf());
        state.push(
            InputLogEvent::builder()
                .message("myMessage1".to_string())
                .timestamp(1)
                .build(),
        );
        assert_eq!(state.uploader.events.len(), 0);
        state.push(
            InputLogEvent::builder()
                .message("myMessage2".to_string())
                .timestamp(1)
                .build(),
        );
        assert_eq!(state.uploader.events.len(), 0);
    }
}
