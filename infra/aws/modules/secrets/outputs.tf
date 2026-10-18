output "secret_arns" {
  description = "Shell ARNs keyed by purpose. Empty when resources are disabled."
  value       = { for key, secret in aws_secretsmanager_secret.this : key => secret.arn }
}
