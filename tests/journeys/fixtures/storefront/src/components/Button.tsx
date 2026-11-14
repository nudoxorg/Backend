import clsx from "clsx";
import type { ButtonHTMLAttributes } from "react";

type Props = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "quiet";
  size?: "sm" | "lg";
};

export function Button({ variant = "quiet", size = "sm", className, ...rest }: Props) {
  return (
    <button
      className={clsx("btn", variant === "primary" && "btn-primary", size === "lg" && "btn-lg", className)}
      {...rest}
    />
  );
}
