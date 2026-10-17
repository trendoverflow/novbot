variable "enable_resources" {
  type        = bool
  description = "Allow extra node instances when true. node_count must also be greater than zero."
  default     = false
}

variable "name_prefix" {
  type        = string
  description = "Name prefix, for example novbot-aws-staging."
}

variable "node_count" {
  type        = number
  description = "How many extra node instances to create. Zero is valid."
  default     = 0
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
  description = "Private subnet ids. Instances round-robin across the list."
  default     = []
}

variable "vpc_security_group_ids" {
  type        = list(string)
  description = "Node security groups."
  default     = []
}

variable "tip_sha" {
  type        = string
  description = "Pinned NovBot git SHA prefix recorded on each instance."
  default     = "7b3f6913"
}
