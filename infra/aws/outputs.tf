output "enable_resources" {
  description = "False in this skeleton. True only after Fei GO and a reviewed tfvars change."
  value       = var.enable_resources
}

output "environment" {
  description = "Target environment name."
  value       = var.environment
}

output "tip_sha" {
  description = "Pinned NovBot git SHA prefix (C2)."
  value       = var.tip_sha
}

output "vpc_id" {
  description = "VPC id, or null while resources are disabled."
  value       = module.network.vpc_id
}

output "alb_dns_name" {
  description = "ALB DNS name, or null while resources are disabled (C1)."
  value       = module.ingress.alb_dns_name
}

output "rds_endpoint" {
  description = "RDS endpoint host, or null while resources are disabled."
  value       = module.data.endpoint
}

output "center_instance_id" {
  description = "Center EC2 id, or null while resources are disabled."
  value       = module.compute_center.instance_id
}

output "node_instance_ids" {
  description = "Optional extra node EC2 ids. Empty when node_count is 0 or resources are disabled."
  value       = module.compute_node.instance_ids
}

output "secret_arns" {
  description = "Secrets Manager shell ARNs. Empty while resources are disabled. Values are never in state from this skeleton."
  value       = module.secrets.secret_arns
}

output "gha_plan_role_arn" {
  description = "ARN of novbot-gha-terraform-plan, or null while resources are disabled."
  value       = module.github_oidc.plan_role_arn
}

output "gha_apply_role_arn" {
  description = "ARN of novbot-gha-terraform-apply, or null while resources are disabled."
  value       = module.github_oidc.apply_role_arn
}
