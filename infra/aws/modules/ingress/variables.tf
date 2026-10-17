variable "enable_resources" {
  type        = bool
  description = "Create the ALB and certificate when true."
  default     = false
}

variable "name_prefix" {
  type        = string
  description = "Name prefix, for example novbot-aws-staging."
}

variable "vpc_id" {
  type        = string
  description = "VPC id. Null while the network module is disabled."
  default     = null
}

variable "public_subnet_ids" {
  type        = list(string)
  description = "Public subnet ids for the ALB. At least two when resources are enabled."
  default     = []
}

variable "alb_security_group_ids" {
  type        = list(string)
  description = "ALB security group ids."
  default     = []
}

variable "certificate_domain_name" {
  type        = string
  description = "ACM certificate domain. Replace the .invalid placeholder before GO."
  default     = "staging.example.invalid"
}

variable "center_instance_id" {
  type        = string
  description = "Center instance id registered with the target group. Null while compute is disabled."
  default     = null
}
