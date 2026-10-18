variable "enable_resources" {
  type        = bool
  description = "Create the VPC and related resources when true."
  default     = false
}

variable "name_prefix" {
  type        = string
  description = "Name prefix, for example novbot-aws-staging."
}

variable "aws_region" {
  type        = string
  description = "Region used in VPC endpoint service names."
}

variable "vpc_cidr" {
  type        = string
  description = "VPC CIDR."
}

variable "availability_zones" {
  type        = list(string)
  description = "AZs. Same length as the subnet CIDR lists. Minimum two so an ALB can be created later."
}

variable "public_subnet_cidrs" {
  type        = list(string)
  description = "Public subnet CIDRs, one per AZ."
}

variable "private_subnet_cidrs" {
  type        = list(string)
  description = "Private subnet CIDRs, one per AZ."
}

variable "alb_ingress_cidrs" {
  type        = list(string)
  description = "CIDRs allowed to open ALB ports 80 and 443. Empty creates no public ingress rule."
  default     = []
}
