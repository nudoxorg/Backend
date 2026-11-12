"""Synthetic fixture projects for the customer-report route checks.

Everything here is written from the symptoms in the reports; no customer code or
data is used. Each builder writes a complete project tree under `root`.
"""
import os
import pathlib

REACT_TYPES = '''\
// Structural stand-in for the public @types/react package: `export =` a namespace that
// owns the recursive ReactNode alias, plus the global JSX namespace.
export = React;
export as namespace React;

declare namespace React {
  type Key = string | number | bigint;
  type JSXElementConstructor<P> =
    | ((props: P) => ReactNode | Promise<ReactNode>)
    | (new (props: P) => Component<any, any>);
  interface ReactElement<P = any, T extends string | JSXElementConstructor<any> = string | JSXElementConstructor<any>> {
    type: T;
    props: P;
    key: string | null;
  }
  class Component<P = {}, S = {}> {
    props: Readonly<P>;
    state: Readonly<S>;
  }
  interface ReactPortal extends ReactElement {
    children: ReactNode;
  }
  type AwaitedReactNode =
    | ReactElement
    | string
    | number
    | bigint
    | Iterable<ReactNode>
    | ReactPortal
    | boolean
    | null
    | undefined;
  type ReactNode =
    | ReactElement
    | string
    | number
    | bigint
    | Iterable<ReactNode>
    | ReactPortal
    | boolean
    | null
    | undefined
    | Promise<AwaitedReactNode>;
  type Dispatch<A> = (value: A) => void;
  type SetStateAction<S> = S | ((previous: S) => S);
  function useState<S>(initial: S | (() => S)): [S, Dispatch<SetStateAction<S>>];
  function useEffect(effect: () => void | (() => void), deps?: readonly unknown[]): void;
  type FC<P = {}> = (props: P) => ReactNode;
}

declare global {
  namespace JSX {
    interface IntrinsicElements {
      [name: string]: any;
    }
    interface ElementChildrenAttribute {
      children: {};
    }
  }
}
'''


def write(root, rel, text):
    path = pathlib.Path(root) / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


def crm_project(root):
    """FastAPI + Vue scaffold (CRM report)."""
    write(root, "backend/main.py", '''\
from fastapi import FastAPI

app = FastAPI(title="crm")


@app.get("/api/health")
def health():
    return {"status": "ok"}
''')
    write(root, "backend/requirements.txt", "fastapi==0.162.0\nuvicorn==0.54.0\n")
    write(root, "frontend/package.json", '''\
{"name":"crm-frontend","version":"0.0.0","private":true,"type":"module",
 "scripts":{"dev":"vite","build":"vite build"},
 "dependencies":{"vue":"3.5.44"},"devDependencies":{"vite":"6.0.0"}}
''')
    write(root, "frontend/vite.config.js",
          "import { defineConfig } from 'vite'\nexport default defineConfig({ server: { port: 5173 } })\n")
    write(root, "frontend/src/main.js",
          "import { createApp } from 'vue'\nimport App from './App.vue'\n\ncreateApp(App).mount('#app')\n")
    write(root, "frontend/src/App.vue", '''\
<script setup>
import { ref, onMounted } from 'vue'

const status = ref('loading')

async function loadHealth() {
  const response = await fetch('/api/health')
  status.value = (await response.json()).status
}

onMounted(loadHealth)
</script>

<template>
  <main>{{ status }}</main>
</template>
''')
    return root


def budget_project(root):
    """ASP.NET minimal API + Next.js style React client (budget reports)."""
    # --- TypeScript / React client -------------------------------------------------
    write(root, "frontend/package.json",
          '{"name":"budget-frontend","version":"1.0.0","private":true}\n')
    write(root, "frontend/tsconfig.json", '''\
{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","strict":true,
 "jsx":"preserve","noEmit":true,"baseUrl":".","paths":{"@/*":["./*"]},
 "typeRoots":["./node_modules/@types"]},
 "include":["**/*.ts","**/*.tsx"],"exclude":["node_modules",".next"]}
''')
    write(root, "frontend/node_modules/@types/react/index.d.ts", REACT_TYPES)
    write(root, "frontend/node_modules/@types/react/package.json",
          '{"name":"@types/react","version":"19.0.0","types":"index.d.ts"}\n')
    write(root, "frontend/node_modules/react/package.json",
          '{"name":"react","version":"19.0.0","main":"index.js"}\n')
    write(root, "frontend/lib/api.ts", '''\
export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

export type User = { id: string; email: string };
export type BankAccount = { id: string; name: string; balance: number };
export type FieldErrors = Record<string, string[]>;

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init);
  if (!response.ok) {
    throw new ApiError(response.status, errorMessage(await response.text()));
  }
  return (await response.json()) as T;
}

export function toFieldErrors(raw: Record<string, string[]>): FieldErrors {
  const fieldErrors: FieldErrors = {};
  for (const key of Object.keys(raw)) fieldErrors[key] = raw[key];
  return fieldErrors;
}

export function me() {
  return request<User>("/api/auth/me");
}

export function listBankAccounts() {
  return request<BankAccount[]>("/api/bank-accounts");
}

export function signOut() {
  return request<void>("/api/auth/signout", { method: "POST" }).catch((e) => errorMessage(e));
}
''')
    methods = "\n".join(
        f"  method{i:03}(input: number): number {{\n    return input + {i};\n  }}\n" for i in range(150))
    write(root, "frontend/lib/longModule.ts",
          "/** Registry of numbered operations; the doc comment is part of the declaration. */\n"
          "export class LongRegistry {\n" + methods + "}\n")
    write(root, "frontend/components/Button.tsx", '''\
import type { ReactNode } from "react";

export function Button({ children, onClick }: { children: ReactNode; onClick?: () => void }) {
  return <button className="btn" onClick={onClick}>{children}</button>;
}
''')
    write(root, "frontend/components/TextField.tsx", '''\
export function TextField({ label, name }: { label: string; name: string }) {
  return <label>{label}<input name={name} /></label>;
}
''')
    write(root, "frontend/components/AuthCard.tsx", '''\
import type { ReactNode } from "react";

export function AuthCard({ title, subtitle, children }: { title: string; subtitle: string; children: ReactNode }) {
  return <section><h1>{title}</h1><p>{subtitle}</p>{children}</section>;
}
''')
    write(root, "frontend/components/AppShell.tsx", '''\
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { me, type User } from "@/lib/api";
import { Button } from "@/components/Button";

export function AppShell({ children }: { children: (user: User) => ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  useEffect(() => {
    me().then(setUser);
  }, []);
  if (!user) return <Button>Loading</Button>;
  return <div>{children(user)}</div>;
}
''')
    write(root, "frontend/components/AuthForm.tsx", '''\
import { ApiError } from "@/lib/api";
import { AuthCard } from "@/components/AuthCard";
import { TextField } from "@/components/TextField";
import { Button } from "@/components/Button";

export function AuthForm() {
  function fail(error: unknown) {
    return error instanceof ApiError ? error.status : 0;
  }
  return (
    <AuthCard title="Sign in" subtitle="Welcome back">
      <TextField label="Email" name="email" />
      <Button onClick={() => fail(null)}>Go</Button>
    </AuthCard>
  );
}
''')
    write(root, "frontend/components/BankAccounts.tsx", '''\
import { useEffect, useState } from "react";
import { listBankAccounts, ApiError, type BankAccount } from "@/lib/api";
import { AppShell } from "@/components/AppShell";
import { Button } from "@/components/Button";
import { TextField } from "@/components/TextField";

export function BankAccounts() {
  const [accounts, setAccounts] = useState<BankAccount[]>([]);
  useEffect(() => {
    listBankAccounts().then(setAccounts).catch((e) => e instanceof ApiError);
  }, []);
  return (
    <AppShell>
      {() => (
        <div>
          <TextField label="Name" name="name" />
          <Button>Add</Button>
          {accounts.length}
        </div>
      )}
    </AppShell>
  );
}
''')
    write(root, "frontend/components/index.ts", 'export { Button as Btn } from "./Button";\n')
    write(root, "frontend/app/page.tsx", '''\
import { Btn } from "@/components";
import { BankAccounts } from "@/components/BankAccounts";

export default function Page() {
  return <BankAccounts />;
}
export const home = () => <Btn>Home</Btn>;
''')
    # a stale build output that must not invalidate indexing
    write(root, "frontend/.next/cache/placeholder.txt", "build cache\n")
    # --- C# minimal API ---------------------------------------------------------------
    write(root, "Budget.slnx", '''\
<Solution>
  <Project Path="backend/Budget.Api/Budget.Api.csproj" />
  <Project Path="backend/Budget.Api.Tests/Budget.Api.Tests.csproj" />
</Solution>
''')
    write(root, "backend/Budget.Api/Budget.Api.csproj", '''\
<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework><Nullable>enable</Nullable></PropertyGroup>
</Project>
''')
    write(root, "backend/Budget.Api.Tests/Budget.Api.Tests.csproj", '''\
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup>
  <ItemGroup><ProjectReference Include="../Budget.Api/Budget.Api.csproj" /></ItemGroup>
</Project>
''')
    write(root, "backend/Budget.Api/Program.cs", '''\
using Budget.Api;

var builder = WebApplication.CreateBuilder(args);
builder.Services.AddDbContext<AppDbContext>();
builder.Services.AddRateLimiter(_ => { });
var app = builder.Build();
app.UseRateLimiter();
app.MapAuthEndpoints();
app.MapBankAccountEndpoints();
app.Run();

public partial class Program;
''')
    write(root, "backend/Budget.Api/AppDbContext.cs", '''\
namespace Budget.Api;

// Uses IdentityUserContext rather than IdentityDbContext: roles are not stored.
public class AppDbContext(DbContextOptions<AppDbContext> options) : IdentityUserContext<AppUser, Guid>(options)
{
    public DbSet<BankAccount> BankAccounts => Set<BankAccount>();
}
''')
    write(root, "backend/Budget.Api/AppUser.cs", "namespace Budget.Api;\n\npublic class AppUser : IdentityUser<Guid>;\n")
    write(root, "backend/Budget.Api/BankAccount.cs", '''\
namespace Budget.Api;

public record BankAccount(Guid Id, Guid UserId, string Name, decimal Balance);
public record BankAccountResponse(Guid Id, string Name, decimal Balance);
''')
    write(root, "backend/Budget.Api/AuthEndpoints.cs", '''\
namespace Budget.Api;

public static class AuthEndpoints
{
    public static void MapAuthEndpoints(this WebApplication app)
    {
        var group = app.MapGroup("/api/auth");
        group.MapPost("/signup", SignUp);
        group.MapGet("/me", Me);
    }

    static IResult SignUp(CredentialsRequest request)
    {
        var errors = MissingFields(request);
        return errors.Count > 0 ? TypedResults.ValidationProblem(errors) : TypedResults.Ok();
    }

    static IResult Me() => TypedResults.Ok();

    static Dictionary<string, string[]> MissingFields(CredentialsRequest request)
    {
        var errors = new Dictionary<string, string[]>();
        if (string.IsNullOrWhiteSpace(request.Email)) errors["email"] = ["required"];
        if (string.IsNullOrWhiteSpace(request.Password)) errors["password"] = ["required"];
        return errors;
    }
}

public record CredentialsRequest(string Email, string Password);
''')
    write(root, "backend/Budget.Api/BankAccountEndpoints.cs", '''\
namespace Budget.Api;

public static class BankAccountEndpoints
{
    public static void MapBankAccountEndpoints(this WebApplication app)
    {
        app.MapGet("/api/bank-accounts", async (AppDbContext db) =>
            await db.BankAccounts.Select(a => new BankAccountResponse(a.Id, a.Name, a.Balance)).ToListAsync());
    }
}
''')
    write(root, "backend/Budget.Api/Migrations/20261103_AddBankAccounts.cs", '''\
namespace Budget.Api.Migrations;

public partial class AddBankAccounts : Migration
{
    protected override void Up(MigrationBuilder migrationBuilder)
    {
        migrationBuilder.CreateTable(name: "BankAccounts", columns: table => new { Id = table.Column<Guid>() });
    }
}
''')
    # a long test class for source paging checks (well over the default page)
    body = "\n".join(
        f'    [Fact]\n    public void Case{i:03}_ReturnsExpectedProblem()\n    {{\n'
        f'        var problem = new Dictionary<string, string[]>();\n'
        f'        Assert.False(problem.ContainsKey("password{i}"));\n    }}\n'
        for i in range(60))
    write(root, "backend/Budget.Api.Tests/AuthEndpointsTests.cs",
          "namespace Budget.Api.Tests;\n\npublic class AuthEndpointsTests\n{\n" + body + "}\n")
    return root


def tiny_ts_project(root):
    write(root, "package.json", '{"name":"tiny","version":"1.0.0","private":true}\n')
    write(root, "tsconfig.json",
          '{"compilerOptions":{"target":"ES2020","module":"ESNext","strict":true,"noEmit":true},"include":["src"]}\n')
    write(root, "src/definition.ts", 'export function greet(name: string): string { return `hello ${name}`; }\n')
    write(root, "src/consumer.ts", 'import { greet } from "./definition";\nexport const greeting = greet("world");\n')
    return root


def refs_project(root, layout="root"):
    """TypeScript reference matrix: every way a declaration is used from another file.

    layout "root": tsconfig at the project root.
    layout "nested": the web app lives in `frontend/` next to a backend directory, as in a
    monorepo whose project root is the repository root.
    layout "gap": root layout plus one component whose props type the checker cannot
    project, so the profile is partial.
    """
    if layout == "nested":
        (pathlib.Path(root) / "backend").mkdir(parents=True, exist_ok=True)
        write(root, "backend/notes.py", "def noted():\n    return 1\n")
        refs_project(pathlib.Path(root) / "frontend", "root")
        return root
    if layout == "mixed":
        refs_project(root, "root")
        write(root, "backend/Program.cs", "var app = WebApplication.CreateBuilder(args).Build();\napp.Run();\n")
        write(root, "backend/Api.csproj", '<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>\n')
        return root
    if layout == "gap":
        refs_project(root, "root")
        write(root, "src/ui/Shell.tsx", '''import type { ReactNode } from "react";
import { aliasTarget } from "@/lib/util";
export function Shell({ children }: { children: (n: number) => ReactNode }) {
  return <div>{children(aliasTarget(3))}</div>;
}
''')
        return root
    write(root, "package.json", '{"name":"refs","version":"1.0.0","private":true}\n')
    write(root, "tsconfig.json", '''{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler",
 "strict":true,"jsx":"preserve","noEmit":true,"baseUrl":".","paths":{"@/*":["./src/*"]},
 "typeRoots":["./node_modules/@types"]},"include":["src"]}
''')
    write(root, "node_modules/@types/react/index.d.ts", REACT_TYPES)
    write(root, "node_modules/@types/react/package.json", '{"name":"@types/react","version":"19.0.0","types":"index.d.ts"}\n')
    write(root, "src/lib/util.ts", '''export function relativeTarget(n: number): number { return n + 1; }
export function aliasTarget(n: number): number { return n + 2; }
export function barrelTarget(n: number): number { return n + 3; }
export type SharedShape = { id: string };
''')
    write(root, "src/lib/index.ts", 'export { barrelTarget } from "./util";\nexport type { SharedShape } from "./util";\n')
    write(root, "src/ui/Widget.tsx", '''export function Widget({ label }: { label: string }) {
  return <span>{label}</span>;
}
''')
    write(root, "src/ui/relative.ts", 'import { relativeTarget } from "../lib/util";\nexport const viaRelative = relativeTarget(1);\n')
    write(root, "src/ui/alias.ts", 'import { aliasTarget } from "@/lib/util";\nexport const viaAlias = aliasTarget(1);\n')
    write(root, "src/ui/barrel.ts", 'import { barrelTarget } from "@/lib";\nimport type { SharedShape } from "@/lib";\nexport const viaBarrel = barrelTarget(1);\nexport const shape: SharedShape = { id: "a" };\n')
    write(root, "src/ui/page.tsx", '''import { Widget } from "@/ui/Widget";
import { aliasTarget } from "@/lib/util";

export function Page() {
  return <div><Widget label="one" /><Widget label={String(aliasTarget(2))} /></div>;
}
''')
    return root


REACT_VARIANTS = {
    "v11_imported_user_type": ('import type { ReactNode } from "react";\nimport type { User } from "./types";\nexport function V11({ children }: { children: (user: User) => ReactNode }) { return <div>{children({ id: "a" })}</div>; }\n'),
    "v12_imported_user_value_import": ('import type { ReactNode } from "react";\nimport { me, type User } from "./types";\nexport function V12({ children }: { children: (user: User) => ReactNode }) { me(); return <div>{children({ id: "a" })}</div>; }\n'),
    "v13_hooks_with_user_state": ('import { useEffect, useState } from "react";\nimport type { ReactNode } from "react";\nimport type { User } from "./types";\nexport function V13({ children }: { children: (user: User) => ReactNode }) { const [user, setUser] = useState<User | null>(null); useEffect(() => { setUser({ id: "a" }); }, []); if (!user) return <div />; return <div>{children(user)}</div>; }\n'),
    "v14_default_react_import": ('import React from "react";\nexport function V14({ children }: { children: React.ReactNode }) { return <div>{children}</div>; }\n'),
    "v15_fc_props": ('import type { FC, ReactNode } from "react";\ninterface P { children: ReactNode }\nexport const V15: FC<P> = ({ children }) => <div>{children}</div>;\n'),
    "v16_function_expression": ('import type { ReactNode } from "react";\nexport const V16 = function ({ children }: { children: ReactNode }) { return <div>{children}</div>; };\n'),
    "v17_alias_import_path": ('import type { ReactNode } from "react";\nimport { me, type User } from "@/lib/types";\nexport function V17({ children }: { children: (user: User) => ReactNode }) { me(); return <div>{children({ id: "a" })}</div>; }\n'),
    "v18_with_component_use": ('import type { ReactNode } from "react";\nimport { Leaf } from "./leaf";\nexport function V18({ children }: { children: ReactNode }) { return <Leaf>{children}</Leaf>; }\n'),

    "v1_plain_value_import": ('import { ReactNode } from "react";\nexport function V1({ children }: { children: ReactNode }) { return <div>{children}</div>; }\n'),
    "v2_plain_type_import": ('import type { ReactNode } from "react";\nexport function V2({ children }: { children: ReactNode }) { return <div>{children}</div>; }\n'),
    "v3_render_prop_type_import": ('import type { ReactNode } from "react";\nexport type User = { id: string };\nexport function V3({ children }: { children: (user: User) => ReactNode }) { return <div>{children({ id: "a" })}</div>; }\n'),
    "v4_render_prop_value_import": ('import { ReactNode } from "react";\ntype User = { id: string };\nexport function V4({ children }: { children: (user: User) => ReactNode }) { return <div>{children({ id: "a" })}</div>; }\n'),
    "v5_named_props": ('import type { ReactNode } from "react";\ninterface Props { title: string; children: ReactNode }\nexport function V5({ title, children }: Props) { return <section>{title}{children}</section>; }\n'),
    "v6_namespace_import": ('import * as React from "react";\nexport function V6({ children }: { children: React.ReactNode }) { return <div>{children}</div>; }\n'),
    "v7_three_props": ('import type { ReactNode } from "react";\nexport function V7({ title, subtitle, children }: { title: string; subtitle: string; children: ReactNode }) { return <section><h1>{title}</h1><p>{subtitle}</p>{children}</section>; }\n'),
    "v8_hooks_and_alias": ('import { useEffect, useState } from "react";\nimport type { ReactNode } from "react";\nexport function V8({ children }: { children: (n: number) => ReactNode }) { const [n, setN] = useState(0); useEffect(() => { setN(1); }, []); return <div>{children(n)}</div>; }\n'),
    "v9_props_param_not_destructured": ('import type { ReactNode } from "react";\nexport function V9(props: { children: ReactNode }) { return <div>{props.children}</div>; }\n'),
    "v10_arrow_component": ('import type { ReactNode } from "react";\nexport const V10 = ({ children }: { children: ReactNode }) => <div>{children}</div>;\n'),
}


def react_variants_project(root):
    write(root, "package.json", '{"name":"react-variants","version":"1.0.0","private":true}\n')
    write(root, "tsconfig.json", '''{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","strict":true,"jsx":"preserve","noEmit":true,"baseUrl":".","paths":{"@/*":["./src/*"]},"typeRoots":["./node_modules/@types"]},"include":["src"]}
''')
    write(root, "node_modules/@types/react/index.d.ts", REACT_TYPES)
    write(root, "node_modules/@types/react/package.json", '{"name":"@types/react","version":"19.0.0","types":"index.d.ts"}\n')
    write(root, "src/types.ts", "export type User = { id: string };\nexport function me(): User { return { id: 'a' }; }\n")
    write(root, "src/lib/types.ts", "export type User = { id: string };\nexport function me(): User { return { id: 'a' }; }\n")
    write(root, "src/leaf.tsx", "export function Leaf({ label }: { label?: string }) { return <i>{label}</i>; }\n")
    for name, text in REACT_VARIANTS.items():
        write(root, f"src/{name}.tsx", text)
    return root


def refs_nested_project(root):
    return refs_project(root, "nested")


def refs_gap_project(root):
    return refs_project(root, "gap")


def refs_mixed_project(root):
    return refs_project(root, "mixed")


def budget_variant(root, drop):
    """The budget project with one ingredient removed, to bisect the cross-file reference misses."""
    import shutil
    budget_project(root)
    base = pathlib.Path(root)
    if drop == "csharp":
        shutil.rmtree(base / "backend")
        (base / "Budget.slnx").unlink()
    elif drop == "appshell":
        (base / "frontend/components/AppShell.tsx").unlink()
        (base / "frontend/components/BankAccounts.tsx").write_text(
            'import { listBankAccounts } from "@/lib/api";\nimport { Button } from "@/components/Button";\n'
            'export function BankAccounts() { listBankAccounts(); return <Button>Add</Button>; }\n')
    elif drop == "next_cache":
        shutil.rmtree(base / "frontend/.next")
    return root


def _bv(drop):
    return lambda root: budget_variant(root, drop)


BUILDERS = {"budget_no_csharp": _bv("csharp"), "budget_no_appshell": _bv("appshell"),
            "budget_no_next": _bv("next_cache"), "refs_mixed": refs_mixed_project, "refs_nested": refs_nested_project, "refs_gap": refs_gap_project, "react_variants": react_variants_project, "refs": refs_project, "crm": crm_project, "budget": budget_project, "tiny": tiny_ts_project}
