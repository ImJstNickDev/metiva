import { createRoot } from "react-dom/client";
import "@fontsource-variable/dm-sans/wght.css";
import "../base.css";
import "./docs.css";
import { Wiki } from "./Wiki";

createRoot(document.getElementById("root")!).render(<Wiki />);
