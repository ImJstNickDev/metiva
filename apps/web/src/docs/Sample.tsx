import { useRef, useState } from "react";
import { Check, Copy, FileJson2, Terminal } from "lucide-react";

export function Sample({
  label,
  text,
  shell = false,
}: {
  label: string;
  text: string;
  shell?: boolean;
}) {
  const code = useRef<HTMLElement>(null);
  const [status, setStatus] = useState("");
  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      setStatus("Copied");
    } catch {
      if (code.current) {
        const range = document.createRange();
        range.selectNodeContents(code.current);
        window.getSelection()?.removeAllRanges();
        window.getSelection()?.addRange(range);
      }
      setStatus("Selected · use Copy");
    }
  }
  const Icon = shell ? Terminal : FileJson2;
  return (
    <div className="sample">
      <div className="sample-bar">
        <span>
          <Icon size={15} />
          {label}
        </span>
        <button type="button" onClick={copy} aria-label={"Copy " + label}>
          {status === "Copied" ? <Check size={14} /> : <Copy size={14} />}
          <span aria-live="polite">{status || "Copy"}</span>
        </button>
      </div>
      <pre tabIndex={0} aria-label={label}>
        <code ref={code}>{text}</code>
      </pre>
    </div>
  );
}
