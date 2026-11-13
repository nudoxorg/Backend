import { useState } from "react";
import { Button } from "./Button";

export function ThemeToggle() {
  const [dark, setDark] = useState(false);
  const toggle = () => {
    const root = document.documentElement;
    const current = getComputedStyle(root).getPropertyValue("--surface");
    root.style.setProperty("--surface", dark ? "#ffffff" : "#101418");
    setDark(!dark && current !== "");
  };
  return <Button onClick={toggle}>Toggle theme</Button>;
}
