export const contractUrl = "/api/v1/openapi.json";

type Security = Record<string, string[]>[];
export type Schema = {
  $ref?: string;
  type?: string | string[];
  format?: string;
  items?: Schema;
  properties?: Record<string, Schema>;
  required?: string[];
  description?: string;
  example?: unknown;
};
type Parameter = {
  name: string;
  in: string;
  required?: boolean;
  description?: string;
  schema: Schema;
  example?: unknown;
};
type ResponseSpec = {
  description: string;
  headers?: Record<
    string,
    { schema: Schema; description?: string; example?: unknown }
  >;
  content?: Record<string, { schema: Schema; example?: unknown }>;
};
type OperationSpec = {
  operationId: string;
  summary: string;
  tags: string[];
  description?: string;
  parameters?: Parameter[];
  security?: Security;
  responses: Record<string, ResponseSpec>;
};
export type Contract = {
  info: { title: string; version: string; description?: string };
  tags?: { name: string; description?: string }[];
  paths: Record<
    string,
    { parameters?: Parameter[]; get?: OperationSpec; head?: OperationSpec }
  >;
  security?: Security;
  components: { schemas: Record<string, Schema> };
};
export type Operation = OperationSpec & {
  path: string;
  method: "GET" | "HEAD";
  parameters: Parameter[];
  security: Security;
};
export function operations(spec: Contract): Operation[] {
  return Object.entries(spec.paths).flatMap(([path, item]) =>
    (["get", "head"] as const).flatMap((verb) => {
      const op = item[verb];
      if (!op) return [];
      const params = new Map(
        (item.parameters ?? []).map((p) => [p.in + "/" + p.name, p]),
      );
      for (const p of op.parameters ?? []) params.set(p.in + "/" + p.name, p);
      return [
        {
          ...op,
          path,
          method: verb === "get" ? ("GET" as const) : ("HEAD" as const),
          parameters: [...params.values()],
          security: op.security ?? spec.security ?? [],
        },
      ];
    }),
  );
}
export function resolveSchema(schema: Schema, spec: Contract): Schema {
  return schema.$ref?.startsWith("#/components/schemas/")
    ? (spec.components.schemas[
        schema.$ref.slice("#/components/schemas/".length)
      ] ?? schema)
    : schema;
}
export function schemaType(schema: Schema): string {
  if (schema.$ref) return schema.$ref.split("/").at(-1)!;
  if (schema.items) return schemaType(schema.items) + "[]";
  if (Array.isArray(schema.type)) return schema.type.join(" | ");
  return schema.format ?? schema.type ?? "JSON";
}
export function requestExample(op: Operation, origin: string) {
  const path = op.path.replace(/\{([^}]+)\}/g, (_, name: string) => {
    const param = op.parameters.find((p) => p.in === "path" && p.name === name);
    return encodeURIComponent(
      String(param?.example ?? param?.schema.example ?? name),
    );
  });
  const option =
    op.method === "HEAD"
      ? " --head"
      : op.responses["200"]?.content?.["application/octet-stream"]
        ? " --output artifact.bin"
        : "";
  return "curl --fail" + option + ' \\\n  "' + origin + path + '"';
}
export const referenceLink = (id: string) => "#/reference/" + id;
