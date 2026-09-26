'use strict';
/*
 * Vendored TypeScript checker oracle driver.
 *
 * Emits one typed JSON report on stdout describing the REAL TypeScript
 * checker's view of exactly one source file:
 *   - one `declarations` entry per source-declared symbol with its
 *     checker type, split into `declared` (a written annotation) and
 *     `computed` (checker inference) origins;
 *   - one `references` entry per resolved identifier use with its
 *     same-file declaration target or foreign module origin, plus the
 *     exact chosen overload index at resolved call sites;
 *   - one optional `narrowings` entry per plain `target = value`
 *     assignment, binding the checker's control-flow-sensitive type of
 *     the assigned value at that exact site to the target's same-file
 *     declaration name span.
 *
 * Every offset is a UTF-16 code-unit offset into the exact source text,
 * which is the native coordinate of the TypeScript compiler API.
 *
 * Resolution requirements mirror the Go oracle's vendored directory: the
 * `typescript` npm package must be resolvable from this script (local or
 * global `node_modules`, or `NODE_PATH`). Exit code 3 reports that the
 * module is unavailable so the Rust adapter can distinguish a missing
 * tool from a malfunction.
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

let ts;
try {
  ts = require('typescript');
} catch (moduleFault) {
  process.stderr.write('checker-driver: the typescript module is not resolvable\n');
  process.exit(3);
}

const sourcePath = process.argv[2];
if (!sourcePath) {
  process.stderr.write('usage: main.cjs <source-file>\n');
  process.exit(2);
}
const absolute = path.resolve(sourcePath);
const bytes = fs.readFileSync(absolute);
const source = bytes.toString('utf8');
const isTsx = absolute.endsWith('.tsx');

const host = ts.createCompilerHost({}, /* setParentNodes */ true);
const program = ts.createProgram(
  [absolute],
  {
    noEmit: true,
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.NodeJs,
    jsx: isTsx ? ts.JsxEmit.Preserve : undefined,
  },
  host,
);
// Every probe below runs against the program's own source-file instance so
// the checker's node-keyed caches observe exactly these nodes.
const sourceFile =
  program.getSourceFile(absolute) ||
  program
    .getSourceFiles()
    .find((file) => path.resolve(file.fileName) === absolute);
if (!sourceFile) {
  process.stderr.write('checker-driver: the source file did not join the program\n');
  process.exit(2);
}
const checker = program.getTypeChecker();

const diagnostics = [];
for (const diagnostic of program.getSyntacticDiagnostics(sourceFile).concat(program.getSemanticDiagnostics(sourceFile))) {
  diagnostics.push(typeof diagnostic.messageText === 'string'
    ? diagnostic.messageText
    : String(diagnostic.messageText.messageText));
}

/** Maps a declaring file to its closed foreign module origin. */
function moduleOf(fileName) {
  const normalized = String(fileName).replace(/\\/g, '/');
  if (/\/lib\.[^/]*\.d\.ts$/.test(normalized)) return 'typescript';
  const marker = normalized.lastIndexOf('/node_modules/');
  if (marker >= 0) {
    const rest = normalized.slice(marker + '/node_modules/'.length);
    const segments = rest.split('/').filter((segment) => segment !== 'node_modules' && segment.length > 0);
    if (segments.length === 0) return null;
    if (segments[0].startsWith('@')) return `${segments[0]}/${segments[1] || ''}`;
    return segments[0];
  }
  return null;
}

/** Returns the source spelling that introduced an imported/exported symbol. */
function moduleSpecifier(node) {
  let parent = node.parent;
  if (parent && ts.isExportSpecifier(parent)) {
    const declaration = parent.parent && parent.parent.parent;
    if (declaration && ts.isExportDeclaration(declaration) && declaration.moduleSpecifier) {
      return declaration.moduleSpecifier.text;
    }
  }
  if (parent && ts.isImportSpecifier(parent)) {
    const declaration = parent.parent && parent.parent.parent;
    if (declaration && ts.isImportDeclaration(declaration) && declaration.moduleSpecifier) {
      return declaration.moduleSpecifier.text;
    }
  }
  return null;
}

/** Maps a resolved foreign file to the import/export specifier that reaches it. */
const spelledSpecifierCache = new Map();

/** Returns the source spelling of an import/export that resolves to `originFile`. */
function spelledSpecifierFor(originFile) {
  const originPath = path.resolve(originFile.fileName);
  if (spelledSpecifierCache.has(originPath)) {
    return spelledSpecifierCache.get(originPath);
  }
  let found = null;
  function consider(specifier) {
    if (found) return;
    const resolved = ts.resolveModuleName(
      specifier,
      sourceFile.fileName,
      program.getCompilerOptions(),
      host,
    );
    if (resolved.resolvedModule) {
      const resolvedPath = path.resolve(resolved.resolvedModule.resolvedFileName);
      if (resolvedPath === originPath) {
        found = specifier;
      }
    }
  }
  function visit(node) {
    if (found) return;
    if (ts.isImportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      consider(node.moduleSpecifier.text);
    }
    if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      consider(node.moduleSpecifier.text);
    }
    ts.forEachChild(node, visit);
  }
  visit(sourceFile);
  spelledSpecifierCache.set(originPath, found);
  return found;
}

/** Reports whether one foreign declaration is a class or interface field. */
function isFieldOrigin(origin) {
  return ts.isPropertyDeclaration(origin) || ts.isPropertySignature(origin);
}

const MAX_TREE_DEPTH = 16;

/** Builds one structured tree for the checker's exact type. */
function typeTree(type, depth) {
  if (type === undefined || type === null) {
    return { kind: 'other', text: safeText(type) };
  }
  if (depth > MAX_TREE_DEPTH) {
    return { kind: 'other', text: safeText(type) };
  }
  const flags = ts.TypeFlags;
  if (type.isThisType === true || type.flags & flags.ThisType) return { kind: 'this' };
  if (type.flags & flags.Union) {
    return { kind: 'union', members: type.types.map((member) => typeTree(member, depth + 1)) };
  }
  if (type.flags & flags.Intersection) {
    return { kind: 'intersection', members: type.types.map((member) => typeTree(member, depth + 1)) };
  }
  if (type.flags & flags.Conditional) {
    return {
      kind: 'conditional',
      check: typeTree(type.checkType, depth + 1),
      extends: typeTree(type.extendsType, depth + 1),
      thenType: typeTree(type.resolvedTrueType || type.trueType, depth + 1),
      elseType: typeTree(type.resolvedFalseType || type.falseType, depth + 1),
    };
  }
  if (type.flags & flags.TemplateLiteral) {
    const texts = type.templateTexts || [];
    const types = type.templateTypes || [];
    const parts = [];
    for (let index = 0; index < texts.length; index += 1) {
      parts.push({ kind: 'text', text: texts[index] });
      if (index < types.length) parts.push({ kind: 'type', type: typeTree(types[index], depth + 1) });
    }
    return { kind: 'templateLiteral', parts };
  }
  if (type.flags & flags.ThisType) return { kind: 'this' };
  if (type.flags & flags.TypeParameter) {
    return { kind: 'typeParameter', name: type.symbol ? type.symbol.getName() : safeText(type) };
  }
  if (type.flags & flags.StringLiteral) return { kind: 'literal', literal: 'string', text: safeText(type) };
  if (type.flags & flags.NumberLiteral) return { kind: 'literal', literal: 'number', text: safeText(type) };
  if (type.flags & flags.BigIntLiteral) return { kind: 'literal', literal: 'bigint', text: safeText(type) };
  if (type.flags & flags.BooleanLiteral) return { kind: 'literal', literal: 'boolean', text: safeText(type) };
  if (type.flags & flags.String) return { kind: 'primitive', name: 'string' };
  if (type.flags & flags.Number) return { kind: 'primitive', name: 'number' };
  if (type.flags & flags.Boolean) return { kind: 'primitive', name: 'boolean' };
  if (type.flags & flags.BigInt) return { kind: 'primitive', name: 'bigint' };
  if (type.flags & (flags.ESSymbol | flags.UniqueESSymbol)) return { kind: 'primitive', name: 'symbol' };
  if (type.flags & flags.Void) return { kind: 'primitive', name: 'void' };
  if (type.flags & flags.Undefined) return { kind: 'primitive', name: 'undefined' };
  if (type.flags & flags.Null) return { kind: 'primitive', name: 'null' };
  if (type.flags & flags.Never) return { kind: 'primitive', name: 'never' };
  if (type.flags & flags.Unknown) return { kind: 'primitive', name: 'unknown' };
  if (type.flags & flags.Any) return { kind: 'primitive', name: 'any' };
  if (type.flags & flags.Object) return objectTypeTree(type, depth + 1);
  if (type.getCallSignatures && type.getCallSignatures().length > 0) {
    return signatureTree(type.getCallSignatures()[0], depth + 1);
  }
  return { kind: 'other', text: safeText(type) };
}

function safeText(type) {
  try {
    return checker.typeToString(type);
  } catch (renderFault) {
    return '<unprintable>';
  }
}

/** Builds the tree of one object type: reference, tuple, or anonymous record. */
function objectTypeTree(type, depth) {
  const objectFlags = type.objectFlags || 0;
  if (objectFlags & ts.ObjectFlags.Mapped) {
    const declaration = type.declaration;
    const parameter = declaration && declaration.typeParameter;
    const constraint = parameter && parameter.constraint;
    const nameAs = declaration && declaration.nameType;
    const value = declaration && declaration.type;
    return {
      kind: 'mapped',
      parameter: parameter && parameter.name ? parameter.name.text : 'K',
      constraint: constraint ? typeTree(checker.getTypeFromTypeNode(constraint), depth) : { kind: 'other', text: 'unknown' },
      nameAs: nameAs ? typeTree(checker.getTypeFromTypeNode(nameAs), depth) : undefined,
      value: value ? typeTree(checker.getTypeFromTypeNode(value), depth) : { kind: 'other', text: 'unknown' },
      readonly: mappedModifier(declaration && declaration.readonlyToken),
      optional: mappedModifier(declaration && declaration.questionToken),
    };
  }
  if (objectFlags & ts.ObjectFlags.Tuple) {
    const elements = (checker.getTypeArguments(type) || []).map((element) => typeTree(element, depth));
    return { kind: 'tuple', elements };
  }
  if (objectFlags & ts.ObjectFlags.Reference) {
    const args = (checker.getTypeArguments(type) || []).map((argument) => typeTree(argument, depth));
    const target = type.target || type;
    const symbol = target.symbol;
    const name = symbol ? symbol.getName() : safeText(type);
    const declarations = symbol && symbol.getDeclarations ? symbol.getDeclarations() || [] : [];
    const origin = declarations[0];
    if (origin) {
      const originFile = origin.getSourceFile ? origin.getSourceFile() : null;
      if (originFile && originFile !== sourceFile) {
        return { kind: 'reference', name, module: moduleOf(originFile.fileName), args };
      }
    } else {
      // A built-in target without a declaration in this program is a
      // language-library type the checker resolved outside the source.
      return { kind: 'reference', name, module: 'typescript', args };
    }
    return { kind: 'reference', name, args };
  }
  // A named nominal type (class, interface, enum, or alias) is reported by
  // reference instead of expanded members: expansion would inline the whole
  // recursive structure of every named type and never terminate on mutual
  // recursion. Only genuinely anonymous object literal types expand.
  const nominal = type.symbol || type.aliasSymbol;
  if (nominal) {
    const declarations = nominal.getDeclarations ? nominal.getDeclarations() || [] : [];
    const origin = declarations[0];
    const originFile = origin && origin.getSourceFile ? origin.getSourceFile() : null;
    if (origin && originFile && originFile !== sourceFile) {
      return { kind: 'reference', name: nominal.getName(), module: moduleOf(originFile.fileName), args: [] };
    }
    if (origin && originFile === sourceFile && isNominalDeclaration(origin)) {
      return { kind: 'reference', name: nominal.getName(), args: [] };
    }
  }
  const members = (type.getProperties() || []).map((property) => ({
    name: property.getName(),
    optional: (property.flags & ts.SymbolFlags.Optional) !== 0,
    readonly: isReadonlyProperty(property),
    type: typeTree(checker.getTypeOfSymbol(property), depth),
  }));
  const callSignatures = type.getCallSignatures ? type.getCallSignatures() : [];
  const constructSignatures = type.getConstructSignatures ? type.getConstructSignatures() : [];
  if (members.length === 0 && callSignatures.length === 1 && constructSignatures.length === 0) {
    return signatureTree(callSignatures[0], depth);
  }
  return { kind: 'object', members };
}

function mappedModifier(token) {
  if (!token) return 'preserve';
  return token.kind === ts.SyntaxKind.MinusToken ? 'remove' : 'add';
}

function isNominalDeclaration(origin) {
  return ts.isInterfaceDeclaration(origin)
    || ts.isClassDeclaration(origin)
    || ts.isEnumDeclaration(origin)
    || ts.isTypeAliasDeclaration(origin);
}

function isReadonlyProperty(property) {
  const declarations = property.getDeclarations ? property.getDeclarations() || [] : [];
  const origin = declarations[0];
  // An `as const` assertion seals every member readonly without writing any
  // `readonly` token, so the assertion chain above the origin declaration is
  // the only honest witness. Parent pointers exist because the compiler host
  // sets parent nodes.
  for (let node = origin; node; node = node.parent) {
    if (ts.isAsExpression(node) && node.type && ts.isTypeReferenceNode(node.type)
      && node.type.typeName.getText(sourceFile) === 'const') return true;
  }
  const checkFlags = ts.CheckFlags || {};
  if (checkFlags.Readonly !== undefined && property.checkFlags !== undefined) {
    return (property.checkFlags & checkFlags.Readonly) !== 0;
  }
  if (origin && ts.isPropertySignature(origin)) return origin.readonly === true;
  if (origin && ts.isPropertyDeclaration(origin)) {
    return origin.modifiers.some((modifier) => modifier.kind === ts.SyntaxKind.ReadonlyKeyword);
  }
  return false;
}

/** Builds the tree of one callable signature. */
function signatureTree(signature, depth) {
  const parameters = (signature.parameters || []).map((parameter) =>
    parameterTree(parameter, depth),
  );
  return { kind: 'function', parameters, result: typeTree(signature.getReturnType(), depth) };
}

function parameterTree(parameter, depth) {
  const declaration = parameter.valueDeclaration || (parameter.getDeclarations && parameter.getDeclarations()[0]);
  const name = declaration && declaration.name ? declaration.name.getText(sourceFile) : null;
  return {
    name: name && name.length > 0 ? name : null,
    optional: (parameter.flags & ts.SymbolFlags.Optional) !== 0 || Boolean(declaration && declaration.questionToken),
    rest: Boolean(declaration && declaration.dotDotDotToken),
    type: typeTree(checker.getTypeOfSymbol(parameter), depth),
  };
}

/** The declaration-name identifier of one declaration node, if any. */
function declaredName(node) {
  if (!node || typeof node.name !== 'object' || node.name === null) return null;
  if (!ts.isIdentifier(node.name) && !ts.isStringLiteral(node.name) && !ts.isNumericLiteral(node.name)) return null;
  return node.name;
}

const declarations = [];
const references = [];
const narrowings = [];

function emitVariableLike(node, annotation) {
  const name = declaredName(node);
  if (!name || !ts.isIdentifier(name)) return;
  const symbol = checker.getSymbolAtLocation(name);
  if (!symbol) return;
  const nameStart = name.getStart(sourceFile);
  const nameEnd = name.getEnd();
  if (annotation) {
    declarations.push({
      nameStart,
      nameEnd,
      origin: 'declared',
      type: typeTree(checker.getTypeFromTypeNode(annotation), 0),
    });
  }
  declarations.push({
    nameStart,
    nameEnd,
    origin: 'computed',
    type: typeTree(checker.getTypeOfSymbolAtLocation(symbol, name), 0),
  });
}

function emitOverloadGroup(node, isDeclarationKind) {
  const name = declaredName(node);
  if (!name) return;
  const symbol = checker.getSymbolAtLocation(name);
  const group = symbol
    ? (symbol.getDeclarations() || []).filter((decl) => isDeclarationKind(decl) && decl.name && decl.name.getText(sourceFile) === name.getText(sourceFile))
    : [node];
  const overloadIndex = Math.max(0, group.indexOf(node));
  const signature = checker.getSignatureFromDeclaration(node);
  const entry = {
    nameStart: name.getStart(sourceFile),
    nameEnd: name.getEnd(),
    origin: 'declared',
    overloadIndex,
  };
  if (signature) {
    entry.type = signatureTree(signature, 0);
    if (group.length === 1) {
      declarations.push(entry);
      declarations.push({
        nameStart: entry.nameStart,
        nameEnd: entry.nameEnd,
        origin: 'computed',
        type: signatureTree(signature, 0),
      });
      return;
    }
  }
  declarations.push(entry);
}

function visit(node) {
  if (ts.isVariableDeclaration(node)) {
    emitVariableLike(node, node.type);
  } else if (ts.isFunctionDeclaration(node) && node.name && node.name.getText(sourceFile).length > 0) {
    emitOverloadGroup(node, ts.isFunctionDeclaration);
  } else if (ts.isParameter(node)) {
    emitVariableLike(node, node.type);
  } else if (ts.isPropertySignature(node) || ts.isPropertyDeclaration(node)) {
    emitVariableLike(node, node.type);
  } else if (ts.isMethodSignature(node) || ts.isMethodDeclaration(node)) {
    emitOverloadGroup(node, (decl) => ts.isMethodSignature(decl) || ts.isMethodDeclaration(decl));
  } else if (ts.isEnumMember(node)) {
    const name = declaredName(node);
    if (name && ts.isIdentifier(name)) {
      const symbol = checker.getSymbolAtLocation(name);
      if (symbol) {
        declarations.push({
          nameStart: name.getStart(sourceFile),
          nameEnd: name.getEnd(),
          origin: 'computed',
          type: typeTree(checker.getTypeOfSymbolAtLocation(symbol, name), 0),
        });
      }
    }
  } else if (ts.isTypeAliasDeclaration(node)) {
    const name = declaredName(node);
    if (name && ts.isIdentifier(name)) {
      declarations.push({
        nameStart: name.getStart(sourceFile),
        nameEnd: name.getEnd(),
        origin: 'declared',
        type: typeTree(checker.getTypeFromTypeNode(node.type), 0),
      });
    }
  } else if (ts.isIdentifier(node)) {
    emitReference(node);
  } else if (ts.isAssignmentExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken) {
    emitNarrowing(node);
  }
  node.forEachChild(visit);
}

/**
 * Emits one control-flow narrowing entry for a plain `target = value`
 * assignment whose target is an identifier declared in this same file.
 * The entry binds the checker's type of the assigned value at this exact
 * program point (the fact the syntax plane cannot see) to the target's
 * declaration-name span, so the consumer can own the row by the declared
 * fact instead of by an ambiguous spelling.
 */
function emitNarrowing(node) {
  if (!ts.isIdentifier(node.left)) return;
  const symbol = checker.getSymbolAtLocation(node.left);
  if (!symbol) return;
  const declarationsOfSymbol = symbol.getDeclarations ? symbol.getDeclarations() || [] : [];
  const origin = declarationsOfSymbol[0];
  const originFile = origin && origin.getSourceFile ? origin.getSourceFile() : null;
  const originName = origin ? declaredName(origin) : null;
  if (originFile !== sourceFile || !originName || !ts.isIdentifier(originName)) return;
  narrowings.push({
    nameStart: originName.getStart(sourceFile),
    nameEnd: originName.getEnd(),
    start: node.left.getStart(sourceFile),
    end: node.getEnd(),
    type: typeTree(checker.getTypeAtLocation(node.right), 0),
  });
}

/** Whether this identifier is the declared name of its parent declaration. */
const DECLARATION_NAME_PARENTS = [
  ts.isVariableDeclaration,
  ts.isParameter,
  ts.isFunctionDeclaration,
  ts.isPropertySignature,
  ts.isPropertyDeclaration,
  ts.isMethodSignature,
  ts.isMethodDeclaration,
  ts.isEnumMember,
  ts.isTypeParameterDeclaration,
  ts.isInterfaceDeclaration,
  ts.isClassDeclaration,
  ts.isEnumDeclaration,
  ts.isTypeAliasDeclaration,
  ts.isModuleDeclaration,
  ts.isImportSpecifier,
  ts.isShorthandPropertyAssignment,
].filter(typeof_test);

function typeof_test(fn) {
  return typeof fn === 'function';
}

function isDeclaredName(node) {
  const parent = node.parent;
  if (!parent || parent.name !== node) return false;
  return DECLARATION_NAME_PARENTS.some((isParentKind) => isParentKind(parent));
}

function emitReference(node) {
  if (isDeclaredName(node)) return;
  let symbol = checker.getSymbolAtLocation(node);
  if (!symbol) return;
  if (symbol.flags & ts.SymbolFlags.Alias) {
    const aliased = checker.getAliasedSymbol(symbol);
    if (aliased) symbol = aliased;
  }
  const declarationsOfSymbol = symbol.getDeclarations ? symbol.getDeclarations() || [] : [];
  const origin = declarationsOfSymbol[0];
  if (!origin) return;
  const entry = { start: node.getStart(sourceFile), end: node.getEnd() };
  const originFile = origin.getSourceFile ? origin.getSourceFile() : null;
  const originName = declaredName(origin);
  if (originFile === sourceFile && originName) {
    entry.targetStart = originName.getStart(sourceFile);
    entry.targetEnd = originName.getEnd();
  } else if (originFile) {
    entry.module = moduleSpecifier(node) || moduleSpecifier(origin) || moduleOf(originFile.fileName) || spelledSpecifierFor(originFile);
    entry.name = originName ? originName.getText(originFile) : symbol.getName();
  } else {
    entry.module = 'typescript';
    entry.name = symbol.getName();
  }
  const parent = node.parent;
  let call = null;
  if (parent && (ts.isCallExpression(parent) || ts.isNewExpression(parent)) && parent.expression === node) {
    call = parent;
  } else if (parent && ts.isPropertyAccessExpression(parent) && parent.name === node) {
    const enclosing = parent.parent;
    if (enclosing && (ts.isCallExpression(enclosing) || ts.isNewExpression(enclosing)) && enclosing.expression === parent) {
      call = enclosing;
    }
  }
  if (call) {
    const resolved = checker.getResolvedSignature(call);
    if (resolved && resolved.declaration) {
      const group = declarationsOfSymbol.filter(
        (decl) => Boolean(decl.name) && ts.isFunctionLike(decl) && checker.getSignatureFromDeclaration(decl) !== undefined,
      );
      const index = group.indexOf(resolved.declaration);
      if (index >= 0) entry.overloadIndex = index;
    }
  }
  if (origin && isFieldOrigin(origin) && parent && ts.isPropertyAccessExpression(parent) && parent.name === node) {
    entry.isField = true;
  }
  references.push(entry);
}

sourceFile.forEachChild(visit);

process.stdout.write(JSON.stringify({
  schemaVersion: 1,
  sourceDigest: crypto.createHash('sha256').update(bytes).digest('hex'),
  diagnostics,
  declarations,
  references,
  narrowings,
}));
