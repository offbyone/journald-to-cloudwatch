mod cloudwatch;
mod configuration;
mod ec2;

use aws_sdk_cloudwatchlogs::model::InputLogEvent;
use chrono::Utc;
use configuration::Configuration;
use std::time::Duration;
use std::{process::exit, thread};
use systemd::{journal, Journal};
use tokio::sync::mpsc::{self, Sender};

fn get_record_timestamp_millis(record: &journal::JournalRecord) -> i64 {
    if let Some(timestamp) = record.get("_SOURCE_REALTIME_TIMESTAMP") {
        if let Ok(timestamp) = timestamp.parse::<i64>() {
            // Convert microseconds to milliseconds
            return timestamp / 1000;
        }
    }
    // Fall back to current time
    Utc::now().timestamp_millis()
}

fn get_record_comm(record: &journal::JournalRecord) -> String {
    if let Some(comm) = record.get("_COMM") {
        comm.to_string()
    } else {
        "unknown".to_string()
    }
}

fn parse_record(record: journal::JournalRecord) -> Option<InputLogEvent> {
    if let Some(message) = record.get("MESSAGE") {
        Some(
            InputLogEvent::builder()
                .message(format!(
                    "{}: {}",
                    get_record_comm(&record),
                    message.to_string()
                ))
                .timestamp(get_record_timestamp_millis(&record))
                .build(),
        )
    } else {
        None
    }
}

fn run_main_loop(conf: Configuration, tx: Sender<InputLogEvent>) {
    match journal::OpenOptions::default()
        .local_only(false)
        .runtime_only(false)
        .open()
    {
        Ok(mut journal) => {
            // Move to the end of the message log
            if let Err(err) = journal.seek(journal::JournalSeek::Tail) {
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

fn short_record(record: &journal::JournalRecord) -> String {
    format!(
        "msg: {}	ts: {}	comm: {}",
        record.get("MESSAGE").unwrap_or(&"nil".to_string()),
        record
            .get("_SOURCE_REALTIME_TIMESTAMP")
            .unwrap_or(&"nil".to_string()),
        record.get("_COMM").unwrap_or(&"nil".to_string())
    )
}

fn handle_journal_entry_loop(
    conf: &Configuration,
    journal: &mut Journal,
    tx: mpsc::Sender<InputLogEvent>,
) {
    let wait_time = Some(Duration::from_secs(1));
    loop {
        match journal.await_next_entry(wait_time) {
            Ok(Some(record)) => {
                conf.debug(format!(
                    "handle_entry: new record: {:?}, tx cap: {}",
                    short_record(&record),
                    tx.capacity(),
                ));
                if let Some(event) = parse_record(record) {
                    if let Err(err) = tx.blocking_send(event) {
                        eprintln!("handle_entry: queue send failed: {}", err);
                    }
                } else {
                    eprintln!("handle_entry: unable to parse the record");
                }
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("handle_entry: await_next_record failed: {}", err)
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let conf = Configuration::new().await;
    let conf2 = conf.clone();
    let (tx, rx) = mpsc::channel(1024);
    let uploader = tokio::spawn(cloudwatch::upload_thread(conf2, rx));

    thread::spawn(move || {
        run_main_loop(conf, tx);
    });
    if let Err(err) = uploader.await {
        eprintln!("join failed: {:?}", err);
    }
}
