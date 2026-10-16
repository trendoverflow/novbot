# Remote state is intentionally not configured.
#
# `terraform init -backend=false` is the supported command for this skeleton.
# Terraform must not contact an S3 backend, and this directory must not gain
# a committed tfstate file.
#
# After Fei GO, bootstrap a new S3 bucket and a DynamoDB lock table, replace
# every placeholder below, uncomment the block, and re-init. Do not point the
# backend at a bucket this project does not own. Do not commit an account id.
#
# terraform {
#   backend "s3" {
#     bucket         = "REPLACE_ME-novbot-tfstate"
#     key            = "aws-staging/terraform.tfstate"
#     region         = "REPLACE_ME"
#     dynamodb_table = "REPLACE_ME-novbot-tfstate-lock"
#     encrypt        = true
#   }
# }
