variable "enable_resources" {
  type        = bool
  description = "Create empty Secrets Manager shells when true."
  default     = false
}

variable "environment" {
  type        = string
  description = "Environment segment of the secret name."
  default     = "aws-staging"
}
