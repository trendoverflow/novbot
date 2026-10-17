variable "enable_resources" {
  type        = bool
  description = "Create the center instance when true."
  default     = false
}

variable "name_prefix" {
  type        = string
  description = "Name prefix, for example novbot-aws-staging."
}

variable "ami_id" {
  type        = string
  description = "AMI id. Null until set at Fei GO."
  default     = null
}

variable "instance_type" {
  type        = string
  description = "EC2 instance type."
  default     = "t4g.small"
}

variable "subnet_ids" {
  type        = list(string)
  description = "Private subnet ids. The instance uses the first."
  default     = []
}

variable "vpc_security_group_ids" {
  type        = list(string)
  description = "Center security groups."
  default     = []
}

variable "tip_sha" {
  type        = string
  description = "Pinned NovBot git SHA prefix recorded on the instance."
  default     = "7b3f6913"
}
