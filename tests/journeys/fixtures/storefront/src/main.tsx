import { createRoot } from "react-dom/client";
import "./styles/theme.css";
import "./styles/global.css";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(<App />);
