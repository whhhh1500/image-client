export type AssetKind = "image" | "video" | "text";

export type NodeCategory = "generate" | "ai_process" | "edit" | "io";

export type NodeStatus = "idle" | "running" | "success" | "error" | "cancelled";

export interface AssetRef {
  id: string;
  kind: AssetKind;
  path: string;
  width?: number;
  height?: number;
  durationS?: number;
  format?: string;
}

export type ParamKind = "text" | "textarea" | "number" | "select" | "file";

export interface ParamField {
  key: string;
  label: string;
  kind: ParamKind;
  options?: string[];
}

export interface Port {
  name: string;
  kind: "image" | "video" | "text" | "params";
  label: string;
  required?: boolean;
}

export interface NodeSpec {
  type: string;
  label: string;
  category: NodeCategory;
  color: string;
  inputs: Port[];
  outputs: Port[];
  paramFields?: ParamField[];
  defaultConfig: Record<string, unknown>;
}

export type WorkflowNodeData = {
  type: string;
  config: Record<string, unknown>;
  status: NodeStatus;
  progress?: number;
  error?: string;
  resultAssets?: AssetRef[];
} & Record<string, unknown>;
