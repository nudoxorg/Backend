// Stand-ins for the zod 4.1.8 modules errors.ts imports (checks.ts, schemas.ts,
// util.ts), reduced to the names the excerpt uses. Not zod's own code.
export type $ZodStringFormats = "email" | "url" | "uuid" | "regex" | "jwt" | "starts_with" | "ends_with" | "includes";
export interface $ZodType {
  readonly _zod: { readonly def: { readonly type: string } };
}
export type Primitive = string | number | symbol | bigint | boolean | null | undefined;
