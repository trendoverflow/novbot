output "vpc_id" {
  description = "VPC id, or null when resources are disabled."
  value       = one(aws_vpc.this[*].id)
}

output "public_subnet_ids" {
  description = "Public subnet ids. Empty when resources are disabled."
  value       = [for subnet in aws_subnet.public : subnet.id]
}

output "private_subnet_ids" {
  description = "Private subnet ids. Empty when resources are disabled."
  value       = [for subnet in aws_subnet.private : subnet.id]
}

output "alb_security_group_ids" {
  description = "ALB security group ids. Empty when resources are disabled."
  value       = aws_security_group.alb[*].id
}

output "center_security_group_ids" {
  description = "Center security group ids. Empty when resources are disabled."
  value       = aws_security_group.center[*].id
}

output "node_security_group_ids" {
  description = "Node security group ids. Empty when resources are disabled."
  value       = aws_security_group.node[*].id
}

output "rds_security_group_ids" {
  description = "RDS security group ids. Empty when resources are disabled."
  value       = aws_security_group.rds[*].id
}
