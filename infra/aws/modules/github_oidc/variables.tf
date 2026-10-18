variable "enable_resources" {
  type        = bool
  description = "Create the GitHub OIDC provider and the two IAM roles when true."
  default     = false
}

variable "github_repository" {
  type        = string
  description = "Repository slug recorded on the role description."
  default     = "trendoverflow/novbot"
}

variable "plan_subject" {
  type        = string
  description = "Exact OIDC sub allowed to assume the plan role."
}

variable "apply_subject" {
  type        = string
  description = "Exact OIDC sub allowed to assume the apply role."
}

variable "thumbprints" {
  type        = list(string)
  description = "Public GitHub OIDC thumbprints."
}
