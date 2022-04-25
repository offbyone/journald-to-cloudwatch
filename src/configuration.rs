use std::env::var;

use crate::ec2;
use aws_config::meta::region::RegionProviderChain;
use aws_types::region::Region;
use aws_types::SdkConfig;

#[derive(Clone, Debug)]
pub struct Configuration {
    pub log_group_name: String,
    pub log_stream_name: String,
    pub is_debug_mode_enabled: bool,
    pub aws_config: SdkConfig,
}

impl Configuration {
    pub async fn new() -> Configuration {
        let region_provider = RegionProviderChain::default_provider()
            .or_else(Region::new("us-west-2"));

        let aws_config =
            aws_config::from_env().region(region_provider).load().await;

        let log_stream_name = get_log_stream_name().await;
        Configuration {
            log_group_name: var("LOG_GROUP_NAME")
                .unwrap_or("journald-to-cloudwatch".to_string()),
            log_stream_name,
            is_debug_mode_enabled: var("DEBUG").is_ok(),
            aws_config,
        }
    }

    pub fn path(&self) -> String {
        format!("{}/{}", self.log_group_name, self.log_stream_name)
    }

    pub fn debug(&self, message: String) {
        if self.is_debug_mode_enabled {
            eprintln!("{}", message);
        }
    }
}

async fn get_log_stream_name() -> String {
    match ec2::get_instance_id().await {
        Ok(id) => id,
        Err(err) => {
            println!("get_instance_id failed: {}", err);
            "not-ec2".to_string()
        }
    }
}
