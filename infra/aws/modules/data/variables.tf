variable "enable_resources" {
  type        = bool
  description = "Create the RDS MySQL instance when true."
  default     = false
}

variable "name_prefix" {
  type        = string
  description = "Name prefix, for example novbot-aws-staging."
}

variable "private_subnet_ids" {
  type        = list(string)
  description = "Private subnet ids for the DB subnet group."
  default     = []
}

variable "vpc_security_group_ids" {
  type        = list(string)
  description = "Security groups for the instance. Center-only MySQL is defined in the network module."
  default     = []
}

variable "instance_class" {
  type        = string
  description = "RDS instance class."
  default     = "db.t4g.micro"
}

variable "allocated_storage_gb" {
  type        = number
  description = "Allocated gp3 storage in GB."
  default     = 20
}

variable "engine_version" {
  type        = string
  description = "MySQL 8 engine version."
  default     = "8.0"
}

variable "multi_az" {
  type        = bool
  description = "Enable Multi-AZ."
  default     = false
}

variable "backup_retention_days" {
  type        = number
  description = "Backup retention in days."
  default     = 1
}

variable "username" {
  type        = string
  description = "Master user name. The password is not a Terraform variable."
  default     = "novbot"
}
