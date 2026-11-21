import { Fragment, useState } from "react";
import { ArrowRight, ChevronDown } from "lucide-react";
import {
  contractUrl,
  referenceLink,
  requestExample,
  resolveSchema,
  schemaType,
  type Contract,
  type Operation,
  type Schema,
} from "./contract";
import { Sample } from "./Sample";
import { guideLink } from "./Guides";

export function groupOperations(ops: Operation[], spec: Contract) {
  const groups = new Map<string, Operation[]>(
    (spec.tags ?? []).map((tag) => [tag.name, []]),
  );
  for (const op of ops.filter((op) => op.method === "GET")) {
    const tag = op.tags?.[0] ?? "Reference";
    const group = groups.get(tag) ?? [];
    group.push(op);
    groups.set(tag, group);
  }
  return [...groups].filter(([, items]) => items.length);
}
export function ReferenceIndex({
  groups,
  ops,
}: {
  groups: [string, Operation[]][];
  ops: Operation[];
}) {
  return (
    <>
      <h1>Endpoint reference</h1>
      <p className="lede">
        Look up request parameters, authentication and responses for this
        mirror.
      </p>
      {groups.map(([tag, group]) => (
        <section className="endpoint-group" key={tag}>
          <h2>{tag}</h2>
          {group.map((op) => (
            <div className="endpoint-row" key={op.operationId}>
              <a href={referenceLink(op.operationId)}>
                <span className="verb">{op.method}</span>
                <span>
                  <strong>{op.summary}</strong>
                  <code>{op.path}</code>
                </span>
              </a>
              {ops
                .filter(
                  (other) => other.path === op.path && other.method === "HEAD",
                )
                .map((other) => (
                  <a
                    className="head-link"
                    key={other.operationId}
                    href={referenceLink(other.operationId)}
                    aria-label={"HEAD · " + op.summary}
                  >
                    HEAD
                  </a>
                ))}
            </div>
          ))}
        </section>
      ))}
      <p>
        <a className="text-link" href={contractUrl}>
          OpenAPI JSON <ArrowRight size={14} />
        </a>
      </p>
    </>
  );
}

function Fields({ schema, spec }: { schema: Schema; spec: Contract }) {
  const root = resolveSchema(schema, spec);
  const record = root.items ? resolveSchema(root.items, spec) : root;
  if (!record.properties) return null;
  return (
    <details className="field-details">
      <summary>
        Response fields <ChevronDown size={15} />
      </summary>
      <dl className="definition-list">
        {Object.entries(record.properties).map(([name, field]) => (
          <div key={name}>
            <dt>
              <code>{name}</code>
              <small>
                {schemaType(field)}
                {record.required?.includes(name) ? "" : " · optional"}
              </small>
            </dt>
            {field.description && <dd>{field.description}</dd>}
          </div>
        ))}
      </dl>
    </details>
  );
}
function Responses({ op, spec }: { op: Operation; spec: Contract }) {
  const [status, setStatus] = useState("200");
  const response = op.responses[status];
  const [media, payload] = Object.entries(response.content ?? {})[0] ?? [];
  const example = payload?.example;
  const headers = Object.entries(response.headers ?? {});
  const responseLine = "HTTP/1.1 " + status;
  return (
    <section>
      <h2>Responses</h2>
      <div className="response-picker">
        <label htmlFor="status">HTTP status</label>
        <select
          id="status"
          value={status}
          onChange={(event) => setStatus(event.target.value)}
        >
          {Object.entries(op.responses).map(([code, res]) => (
            <option key={code} value={code}>
              {code} · {res.description}
            </option>
          ))}
        </select>
      </div>
      <p>{response.description}</p>
      {example !== undefined && media !== "application/octet-stream" ? (
        <Sample
          label={status + " · " + media}
          text={JSON.stringify(example, null, 2)}
        />
      ) : op.operationId === "getOpenApi" && status === "200" ? (
        <Sample
          label="200 · JSON excerpt"
          text={JSON.stringify({ info: spec.info }, null, 2)}
        />
      ) : (
        <Sample
          label={status + " · response"}
          text={[
            responseLine,
            ...(media ? ["Content-Type: " + media] : []),
            ...headers
              .filter(
                ([, h]) =>
                  h.example !== undefined || h.schema.example !== undefined,
              )
              .map(
                ([name, h]) => name + ": " + (h.example ?? h.schema.example),
              ),
            ...(media === "application/octet-stream"
              ? ["", "[artifact bytes]"]
              : []),
          ].join("\n")}
        />
      )}
      {op.method === "HEAD" && (
        <p>HEAD returns the response headers with an empty body.</p>
      )}
      {payload && <Fields schema={payload.schema} spec={spec} />}
      {!!headers.length && (
        <details className="field-details">
          <summary>
            Response headers <ChevronDown size={15} />
          </summary>
          <dl className="definition-list">
            {headers.map(([name, h]) => (
              <div key={name}>
                <dt>
                  <code>{name}</code>
                  <small>{schemaType(h.schema)}</small>
                </dt>
                {h.description && <dd>{h.description}</dd>}
              </div>
            ))}
          </dl>
        </details>
      )}
      {Object.keys(op.responses).some((code) => Number(code) >= 400) && (
        <>
          <h3>Error responses</h3>
          <dl className="error-list">
            {Object.entries(op.responses)
              .filter(([code]) => Number(code) >= 400)
              .map(([code, res]) => (
                <div key={code}>
                  <dt>{code}</dt>
                  <dd>{res.description}</dd>
                </div>
              ))}
          </dl>
        </>
      )}
    </section>
  );
}
export function Endpoint({
  op,
  ops,
  spec,
  origin,
}: {
  op: Operation;
  ops: Operation[];
  spec: Contract;
  origin: string;
}) {
  const siblings = ops.filter((item) => item.path === op.path);
  const allowsBearer = op.security.some((item) =>
    Object.hasOwn(item, "bearerKey"),
  );
  const allowsPublic =
    !op.security.length ||
    op.security.some((item) => !Object.keys(item).length);
  return (
    <>
      <h1>{op.summary}</h1>
      {op.description && <p className="lede">{op.description}</p>}
      <div className="route-box">
        <span className="verb">{op.method}</span>
        <code>
          {op.path
            .split("/")
            .slice(1)
            .map((part, i) => (
              <Fragment key={i}>
                <wbr />/{part}
              </Fragment>
            ))}
        </code>
      </div>
      {siblings.length > 1 && (
        <nav aria-label="Request method" className="method-nav">
          Request method
          {siblings.map((item) => (
            <a
              key={item.method}
              href={referenceLink(item.operationId)}
              aria-current={item.method === op.method ? "page" : undefined}
            >
              {item.method}
            </a>
          ))}
        </nav>
      )}
      <h2>Request</h2>
      <Sample shell label="curl" text={requestExample(op, origin)} />
      <div className="auth-note">
        <strong>Authentication</strong>
        <p>
          {allowsBearer ? (
            <>
              {allowsPublic && "Public content is readable anonymously. "}Use a
              scoped bearer key for private content.{" "}
              <a href={guideLink("private-download")}>Read the key guide</a>.
            </>
          ) : (
            "This operation is publicly readable."
          )}
        </p>
      </div>
      <h2>Parameters</h2>
      {op.parameters.length ? (
        <dl className="definition-list">
          {op.parameters.map((param) => (
            <div key={param.in + param.name}>
              <dt>
                <code>{param.name}</code>
                <small>
                  {param.in} · {schemaType(param.schema)} ·{" "}
                  {param.required ? "required" : "optional"}
                </small>
              </dt>
              <dd>
                {param.description}
                {param.example !== undefined && (
                  <span className="param-example">
                    Example <code>{String(param.example)}</code>
                  </span>
                )}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p>This request takes no parameters.</p>
      )}
      <Responses op={op} spec={spec} />
    </>
  );
}
