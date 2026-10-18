# Wiring only. Each module creates zero AWS resources while enable_resources
# is false (the default). Do not terraform apply this root until Fei GO.

locals {
  name_prefix = "novbot-${var.environment}"
}

module "network" {
  source = "./modules/network"

  enable_resources     = var.enable_resources
  name_prefix          = local.name_prefix
  aws_region           = var.aws_region
  vpc_cidr             = var.vpc_cidr
  availability_zones   = var.availability_zones
  public_subnet_cidrs  = var.public_subnet_cidrs
  private_subnet_cidrs = var.private_subnet_cidrs
  alb_ingress_cidrs    = var.alb_ingress_cidrs
}

module "data" {
  source = "./modules/data"

  enable_resources       = var.enable_resources
  name_prefix            = local.name_prefix
  private_subnet_ids     = module.network.private_subnet_ids
  vpc_security_group_ids = module.network.rds_security_group_ids
  instance_class         = var.rds_instance_class
  allocated_storage_gb   = var.rds_allocated_storage_gb
  engine_version         = var.rds_engine_version
  multi_az               = var.rds_multi_az
  backup_retention_days  = var.rds_backup_retention_days
  username               = var.db_username
}

module "compute_center" {
  source = "./modules/compute_center"

  enable_resources       = var.enable_resources
  name_prefix            = local.name_prefix
  ami_id                 = var.ami_id
  instance_type          = var.center_instance_type
  subnet_ids             = module.network.private_subnet_ids
  vpc_security_group_ids = module.network.center_security_group_ids
  tip_sha                = var.tip_sha
}

module "compute_node" {
  source = "./modules/compute_node"

  enable_resources       = var.enable_resources
  name_prefix            = local.name_prefix
  node_count             = var.node_count
  ami_id                 = var.ami_id
  instance_type          = var.node_instance_type
  subnet_ids             = module.network.private_subnet_ids
  vpc_security_group_ids = module.network.node_security_group_ids
  tip_sha                = var.tip_sha
}

module "ingress" {
  source = "./modules/ingress"

  enable_resources        = var.enable_resources
  name_prefix             = local.name_prefix
  vpc_id                  = module.network.vpc_id
  public_subnet_ids       = module.network.public_subnet_ids
  alb_security_group_ids  = module.network.alb_security_group_ids
  certificate_domain_name = var.certificate_domain_name
  center_instance_id      = module.compute_center.instance_id
}

module "secrets" {
  source = "./modules/secrets"

  enable_resources = var.enable_resources
  environment      = var.environment
}

module "github_oidc" {
  source = "./modules/github_oidc"

  enable_resources  = var.enable_resources
  github_repository = var.github_repository
  plan_subject      = var.github_plan_subject
  apply_subject     = var.github_apply_subject
  thumbprints       = var.github_oidc_thumbprints
}
