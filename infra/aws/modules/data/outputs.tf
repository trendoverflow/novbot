output "endpoint" {
  description = "RDS endpoint host, or null when the instance is disabled."
  value       = one(aws_db_instance.mysql[*].address)
}

output "port" {
  description = "RDS port, or null when the instance is disabled."
  value       = one(aws_db_instance.mysql[*].port)
}

output "db_name" {
  description = "Database name, or null when the instance is disabled."
  value       = one(aws_db_instance.mysql[*].db_name)
}
