# data

RDS MySQL 8 for the center, in private subnets, encrypted, not publicly accessible.

The master password is not in Terraform. `manage_master_user_password` tells RDS to store it in Secrets Manager when the instance is later created. The center database URL is a separate shell in the `secrets` module and is filled out of band.

Single-AZ and a one-day backup retention are staging placeholders so a database can be restored far enough to smoke-test. They are not a production policy.

The instance is created only when `enable_resources` is true. Default is false.
