export * from "../deployments.js";

export type DeploymentStatus = "active" | "archived";

export interface DeploymentRecordMetadata {
  status: DeploymentStatus;
  deprecatedAt?: string;
  deprecationReason?: string;
  successorId?: string;
}

export interface DeprecationWarning {
  code: "RETIRED_ROUTER";
  recordId: string;
  message: string;
  successorId?: string;
}

export function isActiveDeployment(meta: DeploymentRecordMetadata): boolean {
  return meta.status === "active";
}

export function deprecationWarningFor(
  recordId: string,
  meta: DeploymentRecordMetadata,
): DeprecationWarning | undefined {
  if (meta.status !== "archived") {
    return undefined;
  }
  const successorHint = meta.successorId
    ? ` Use successor "${meta.successorId}" instead.`
    : " No successor is recorded; do not activate or monitor this record.";
  return {
    code: "RETIRED_ROUTER",
    recordId,
    message: `Deployment "${recordId}" is archived and must not be activated or monitored.${successorHint}`,
    ...(meta.successorId === undefined ? {} : { successorId: meta.successorId }),
  };
}
