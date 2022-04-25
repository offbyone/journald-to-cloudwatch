mod cloudwatch;
mod configuration;
mod ec2;

use aws_sdk_cloudwatchlogs::model::InputLogEvent;
use chrono::Utc;
use configuration::Configuration;
use std::process::exit;
use systemd::journal::{Journal, JournalFiles, JournalRecord, JournalSeek};
use tokio::sync::mpsc::{self, UnboundedSender};

fn get_record_timestamp_millis(record: &JournalRecord) -> i64 {
    if let Some(timestamp) = record.get("_SOURCE_REALTIME_TIMESTAMP") {
        if let Ok(timestamp) = timestamp.parse::<i64>() {
            // Convert microseconds to milliseconds
            return timestamp / 1000;
        }
    }
    // Fall back to current time
    Utc::now().timestamp_millis()
}

fn get_record_comm(record: &JournalRecord) -> &str {
    if let Some(comm) = record.get("_COMM") {
        comm
    } else {
        "unknown"
    }
}

fn parse_record(record: &JournalRecord) -> Option<InputLogEvent> {
    if let Some(message) = record.get("MESSAGE") {
        Some(
            InputLogEvent::builder()
                .message(format!(
                    "{}: {}",
                    get_record_comm(record),
                    message.to_string()
                ))
                .timestamp(get_record_timestamp_millis(record))
                .build(),
        )
    } else {
        None
    }
}

async fn run_main_loop(
    conf: Configuration,
    tx: UnboundedSender<InputLogEvent>,
) {
    let runtime_only = false;
    let local_only = false;
    match Journal::open(JournalFiles::All, runtime_only, local_only) {
        Ok(mut journal) => {
            // Move to the end of the message log
            if let Err(err) = journal.seek(JournalSeek::Tail) {
                eprintln!("failed to seek to tail: {}", err);
            }

            handle_journal_entry_loop(&conf, &mut journal, tx)
        }
        Err(err) => {
            eprintln!("failed to open journal: {}", err);
            exit(1);
        }
    }
}

fn handle_journal_entry_loop(
    conf: &Configuration,
    journal: &mut Journal,
    tx: mpsc::UnboundedSender<InputLogEvent>,
) {
    let wait_time = None;
    loop {
        match journal.await_next_entry(wait_time) {
            Ok(Some(record)) => {
                conf.debug(format!("new record: {:?}", &record));
                if let Some(event) = parse_record(&record) {
                    if let Err(err) = tx.send(event) {
                        eprintln!("queue send failed: {}", err);
                    }
                }
            }
            Ok(None) => {}
            Err(err) => eprintln!("await_next_record failed: {}", err),
        }
    }
}

#[tokio::main]
async fn main() {
    let conf = Configuration::new().await;
    let conf2 = conf.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let uploader = tokio::spawn(cloudwatch::upload_thread(conf2, rx));
    if let Err(err) = tokio::spawn(run_main_loop(conf, tx)).await {
        eprintln!(
            "Unexpected error running the journal consumer loop: {:?}",
            err
        );
    }
    if let Err(err) = uploader.await {
        eprintln!("join failed: {:?}", err);
    }
}
