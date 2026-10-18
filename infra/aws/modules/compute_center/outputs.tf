output "instance_id" {
  description = "Center instance id, or null when resources are disabled."
  value       = one(aws_instance.center[*].id)
}

output "role_arn" {
  description = "Center IAM role ARN, or null when resources are disabled."
  value       = one(aws_iam_role.center[*].arn)
}
