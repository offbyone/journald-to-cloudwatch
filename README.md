# journald-to-cloudwatch

This is a simple service that copies logs from journald to AWS CloudWatch Logs.

The implementation is very basic. It does not copy logs that were created prior
to journald-to-cloudwatch starting. It has only one configuration option, which
is the name of the log group. The log stream name is derived from the instance
ID (the service assumes it is running on an EC2 instance) or `not-ec2` if it's
not running on an EC2 instance.

## Development

The build scripts use [invoke](https://pyinvoke.org/) to run. You'll need
`invoke` and `tomli` for them, which you can get by `pip install requirements.txt`

To build the service for EC2:

    inv publish-ubuntu

or

    inv publish-amazonlinux2

This builds in a Docker container that has the libraries the EC2 instance would
have.

The output is `dist/journald-to-cloudwatch-{version}.tar.gz`. Copy that to an
EC2 instance. There is an example service configuration file in the tarball.
Copy that to `/etc/systemd/system/` and modify `LOG_GROUP_NAME` to the name of
your log group. Note that the log group must exist for the service to work; it
will not create the log group.

## IAM policy

The following permissions are required:

    logs:CreateLogStream
    logs:DescribeLogStreams
    logs:PutLogEvents
