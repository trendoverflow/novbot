# Skeleton provider. skip_aws_api_checks keeps a local terraform validate
# (and an accidental plan) from calling STS or IMDS. It is not permission
# to plan or apply a real account. Leave the skips on until Fei GO.

provider "aws" {
  region = var.aws_region

  skip_credentials_validation = var.skip_aws_api_checks
  skip_requesting_account_id  = var.skip_aws_api_checks
  skip_metadata_api_check     = var.skip_aws_api_checks

  default_tags {
    tags = {
      Project     = "novbot"
      Environment = var.environment
      ManagedBy   = "terraform"
      TipSha      = var.tip_sha
    }
  }
}
