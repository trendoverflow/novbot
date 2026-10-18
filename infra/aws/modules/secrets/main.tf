# Secret shells only. There is no aws_secretsmanager_secret_version in this
# module, so Terraform never stores a Bearer token, bootstrap token, or
# database URL. Values are written out of band after Fei GO.
#
# Names are cloud-specific. Do not reuse OrbStack demo token names.

locals {
  secrets = {
    api_token = {
      name        = "novbot/${var.environment}/api-token"
      description = "Cloud HTTP Bearer for /v1. Independent of any demo token."
    }
    bootstrap_token = {
      name        = "novbot/${var.environment}/bootstrap-token"
      description = "Node registration bootstrap token for aws-staging."
    }
    database_url = {
      name        = "novbot/${var.environment}/database-url"
      description = "Center MySQL URL. The password comes from the RDS-managed secret, not from git."
    }
  }
}

resource "aws_secretsmanager_secret" "this" {
  for_each = var.enable_resources ? local.secrets : {}

  name                    = each.value.name
  description             = each.value.description
  recovery_window_in_days = 7

  tags = {
    Name = each.value.name
  }
}
