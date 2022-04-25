use reqwest::ClientBuilder;
use std::time::Duration;

/// Use the link-local interface to get the instance ID
///
/// Reference:
/// docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-instance-metadata.html
pub async fn get_instance_id() -> reqwest::Result<String> {
    let client = ClientBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()?;
    let url = "http://169.254.169.254/latest/meta-data/instance-id";
    let response = client.get(url).send().await;
    response?.error_for_status()?.text().await
}
