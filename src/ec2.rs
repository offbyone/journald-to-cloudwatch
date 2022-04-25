use aws_sdk_ec2::error::DescribeInstancesError;
use aws_sdk_ec2::types::SdkError;
use aws_sdk_ec2::Client;
use aws_types::SdkConfig;
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

#[derive(Debug)]
pub enum InstanceNameError {
    DescribeInstancesError(SdkError<DescribeInstancesError>),
    MissingReservations,
    EmptyReservations,
    MissingInstances,
    EmptyInstances,
    MissingTags,
    MissingNameTag,
}

/// Use the instance ID to look up the instance's name tag
pub async fn get_instance_name(
    sdk_config: SdkConfig,
    instance_id: String,
) -> Result<String, InstanceNameError> {
    let client = Client::new(&sdk_config);
    let result = client
        .describe_instances()
        .set_instance_ids(Some(vec![instance_id]))
        .send()
        .await;

    match result {
        Ok(result) => {
            if let Some(reservations) = result.reservations {
                if let Some(reservation) = reservations.first() {
                    if let Some(instances) = &reservation.instances {
                        if let Some(instance) = instances.first() {
                            if let Some(tags) = &instance.tags {
                                for tag in tags.iter() {
                                    if tag.key == Some("Name".to_string()) {
                                        if let Some(value) = &tag.value {
                                            return Ok(value.clone());
                                        }
                                    }
                                }
                                return Err(InstanceNameError::MissingNameTag);
                            } else {
                                return Err(InstanceNameError::MissingTags);
                            }
                        } else {
                            return Err(InstanceNameError::EmptyInstances);
                        }
                    } else {
                        return Err(InstanceNameError::MissingInstances);
                    }
                } else {
                    return Err(InstanceNameError::EmptyReservations);
                }
            } else {
                return Err(InstanceNameError::MissingReservations);
            }
        }
        Err(err) => Err(InstanceNameError::DescribeInstancesError(err)),
    }
}
