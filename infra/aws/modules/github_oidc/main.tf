# GitHub Actions OIDC. Two roles, no long-lived access keys.
# Permissions are sts:GetCallerIdentity only. That cannot plan or apply
# real infrastructure. Replace the policy under NovSec review before Fei GO.
# Do not attach AdministratorAccess, PowerUserAccess, or an action wildcard.

data "aws_iam_policy_document" "plan_trust" {
  count = var.enable_resources ? 1 : 0

  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github[0].arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = [var.plan_subject]
    }
  }
}

data "aws_iam_policy_document" "apply_trust" {
  count = var.enable_resources ? 1 : 0

  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github[0].arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = [var.apply_subject]
    }
  }
}

data "aws_iam_policy_document" "skeleton" {
  count = var.enable_resources ? 1 : 0

  statement {
    sid       = "SkeletonIdentityOnly"
    actions   = ["sts:GetCallerIdentity"]
    resources = ["*"]
  }
}

resource "aws_iam_openid_connect_provider" "github" {
  count = var.enable_resources ? 1 : 0

  url             = "https://token.actions.githubusercontent.com"
  client_id_list  = ["sts.amazonaws.com"]
  thumbprint_list = var.thumbprints

  tags = {
    Name = "github-actions"
  }
}

resource "aws_iam_role" "plan" {
  count = var.enable_resources ? 1 : 0

  name               = "novbot-gha-terraform-plan"
  description        = "GitHub Actions plan role for ${var.github_repository}. Stub permissions only."
  assume_role_policy = data.aws_iam_policy_document.plan_trust[0].json

  tags = {
    Name = "novbot-gha-terraform-plan"
  }
}

resource "aws_iam_role" "apply" {
  count = var.enable_resources ? 1 : 0

  name               = "novbot-gha-terraform-apply"
  description        = "GitHub Actions apply role for ${var.github_repository}. Stub permissions only until NovSec review and Fei GO."
  assume_role_policy = data.aws_iam_policy_document.apply_trust[0].json

  tags = {
    Name = "novbot-gha-terraform-apply"
  }
}

resource "aws_iam_role_policy" "plan" {
  count = var.enable_resources ? 1 : 0

  name   = "skeleton-identity-only"
  role   = aws_iam_role.plan[0].id
  policy = data.aws_iam_policy_document.skeleton[0].json
}

resource "aws_iam_role_policy" "apply" {
  count = var.enable_resources ? 1 : 0

  name   = "skeleton-identity-only"
  role   = aws_iam_role.apply[0].id
  policy = data.aws_iam_policy_document.skeleton[0].json
}
