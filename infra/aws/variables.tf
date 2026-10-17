variable "aws_region" {
  type        = string
  description = "AWS region placeholder. Fei confirms the real region before GO. Default is not an account decision."
  default     = "ap-east-1"
}

variable "environment" {
  type        = string
  description = "First cloud environment. Production is a separate Fei GO and is not created here."
  default     = "aws-staging"

  validation {
    condition     = var.environment == "aws-staging"
    error_message = "This skeleton only supports aws-staging."
  }
}

variable "enable_resources" {
  type        = bool
  description = "Create AWS resources when true. Must stay false in git until Fei GO. Apply is still a separate workflow gate."
  default     = false
}

variable "skip_aws_api_checks" {
  type        = bool
  description = "Skip STS, account-id, and IMDS calls in the AWS provider. Keep true until a real plan is approved."
  default     = true
}

variable "tip_sha" {
  type        = string
  description = "NovBot git SHA prefix pinned for C2. Full tip is 7b3f6913e8ffd2d7ec1e1ccfa38ca583616b438a."
  default     = "7b3f6913"
}

variable "vpc_cidr" {
  type        = string
  description = "Staging VPC CIDR placeholder."
  default     = "10.20.0.0/16"
}

variable "availability_zones" {
  type        = list(string)
  description = "Two AZs for the ALB. Placeholders in the default region."
  default     = ["ap-east-1a", "ap-east-1b"]
}

variable "public_subnet_cidrs" {
  type        = list(string)
  description = "Public subnet CIDRs, one per availability zone. ALB only."
  default     = ["10.20.0.0/24", "10.20.1.0/24"]
}

variable "private_subnet_cidrs" {
  type        = list(string)
  description = "Private subnet CIDRs, one per availability zone. Center, nodes, and RDS."
  default     = ["10.20.10.0/24", "10.20.11.0/24"]
}

variable "alb_ingress_cidrs" {
  type        = list(string)
  description = "Client CIDRs allowed to reach the ALB on 80 and 443. Empty means no public ingress rule. Fei must accept any non-empty list before GO. Do not default this to 0.0.0.0/0."
  default     = []
}

variable "certificate_domain_name" {
  type        = string
  description = "DNS name for the staging ACM certificate. The .invalid name must be replaced before GO and must not be applied as-is."
  default     = "staging.example.invalid"
}

variable "ami_id" {
  type        = string
  description = "AMI for center and optional nodes (arm64 for t4g, x86_64 for t3). Null until Fei GO. Not an account id."
  default     = null
}

variable "center_instance_type" {
  type        = string
  description = "Center EC2 size placeholder. Draft suggestion is t4g.small (t3.small if x86)."
  default     = "t4g.small"
}

variable "node_instance_type" {
  type        = string
  description = "Optional extra node EC2 size placeholder."
  default     = "t4g.small"
}

variable "node_count" {
  type        = number
  description = "Extra node EC2 instances. 0 is the first-land shape: one node co-located on the center instance. Cap is 2 (orb-arm-2/3 alignment)."
  default     = 0

  validation {
    condition     = var.node_count >= 0 && var.node_count <= 2
    error_message = "node_count must be 0, 1, or 2."
  }
}

variable "rds_instance_class" {
  type        = string
  description = "RDS MySQL instance class placeholder."
  default     = "db.t4g.micro"
}

variable "rds_allocated_storage_gb" {
  type        = number
  description = "RDS gp3 size in GB. Placeholder for a smokeable staging database."
  default     = 20
}

variable "rds_engine_version" {
  type        = string
  description = "RDS MySQL 8 engine version. Confirm a concrete minor at GO."
  default     = "8.0"
}

variable "rds_multi_az" {
  type        = bool
  description = "RDS Multi-AZ. Off for the first staging cut."
  default     = false
}

variable "rds_backup_retention_days" {
  type        = number
  description = "RDS backup retention placeholder so a staging DB can be restored for a smoke test. Not a production policy."
  default     = 1
}

variable "db_username" {
  type        = string
  description = "RDS master user name. The password is AWS-managed in Secrets Manager when resources are enabled. Do not put a password in tfvars."
  default     = "novbot"
}

variable "github_repository" {
  type        = string
  description = "GitHub repository allowed to assume the plan and apply roles."
  default     = "trendoverflow/novbot"
}

variable "github_plan_subject" {
  type        = string
  description = "OIDC sub for novbot-gha-terraform-plan. If the repo uses immutable subject claims (owner and repository ids), replace this before GO. Do not widen it to a wildcard."
  default     = "repo:trendoverflow/novbot:pull_request"
}

variable "github_apply_subject" {
  type        = string
  description = "OIDC sub for novbot-gha-terraform-apply. Matches GitHub Environment aws-staging. Replace with the immutable subject form if required. Do not use a wildcard."
  default     = "repo:trendoverflow/novbot:environment:aws-staging"
}

variable "github_oidc_thumbprints" {
  type        = list(string)
  description = "Public GitHub Actions OIDC thumbprints (not secrets). Confirm against current GitHub and AWS docs before GO."
  default = [
    "6938fd4d98bab03faadb97b34396831e3780aea1",
    "1c58a3a8518e8759bf075b76b750d4f2df264fcd",
  ]
}
