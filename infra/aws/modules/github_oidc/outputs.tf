output "plan_role_arn" {
  description = "Plan role ARN, or null when resources are disabled."
  value       = one(aws_iam_role.plan[*].arn)
}

output "apply_role_arn" {
  description = "Apply role ARN, or null when resources are disabled."
  value       = one(aws_iam_role.apply[*].arn)
}

output "oidc_provider_arn" {
  description = "GitHub OIDC provider ARN, or null when resources are disabled."
  value       = one(aws_iam_openid_connect_provider.github[*].arn)
}
