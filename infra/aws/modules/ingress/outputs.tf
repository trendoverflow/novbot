output "alb_dns_name" {
  description = "ALB DNS name, or null when resources are disabled."
  value       = one(aws_lb.this[*].dns_name)
}

output "alb_zone_id" {
  description = "ALB hosted zone id, or null when resources are disabled."
  value       = one(aws_lb.this[*].zone_id)
}

output "certificate_arn" {
  description = "ACM certificate ARN, or null when resources are disabled."
  value       = one(aws_acm_certificate.this[*].arn)
}
