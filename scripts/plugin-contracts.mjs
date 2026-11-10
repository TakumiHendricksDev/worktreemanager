// Derive the author-facing wire schemas from the typed entrypoints wtm itself calls: the
// `invoke` wrappers in src/lib/ipc/commands.ts and `WorkbenchOperations` in src/lib/extensions.ts,
// for public operations only. `catalog.json` is the hand-written input that says which are public.
// A stale generated contract fails verification rather than teaching agents an invented API.
//
// `$defs` are named after the TypeScript type (`AgentEvent`), or after where an anonymous one is
// used (`code_read_file.arguments`). They used to be `t1`…`tN` in traversal order, so one changed
// type renumbered every `$ref` after it, and a guide or an agent that had followed `t173` to a
// file read was pointed at something else by the next build.
import fs from 'node:fs';
import ts from 'typescript';

const config = ts.readConfigFile('tsconfig.json', ts.sys.readFile);
const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, '.');
const program = ts.createProgram(parsed.fileNames, parsed.options);
const checker = program.getTypeChecker();
const catalog = JSON.parse(
    fs.readFileSync('crates/wtm-plugins/contracts/catalog.json', 'utf8'),
);
const publicOperations = new Set(
    catalog.operations.filter((o) => o.exposure === 'public').map((o) => o.id),
);
const definitions = {};
const named = new Map();
/** Utility aliases name a shape only together with their arguments, so they do not name a def. */
const GENERIC = new Set(['Record', 'Pick', 'Omit', 'Partial', 'Required', 'Readonly']);

/** A stable def name: the type's own, or `hint` for an anonymous one, made unique. */
function defName(type, hint) {
    const symbol = type.aliasSymbol ?? type.getSymbol();
    const own =
        symbol &&
        !symbol.name.startsWith('__') &&
        !GENERIC.has(symbol.name) &&
        !(type.aliasTypeArguments?.length || type.typeArguments?.length)
            ? symbol.name
            : null;
    const base = (own ?? hint).replace(/[^A-Za-z0-9_.-]/g, '_');
    let name = base;
    for (let n = 2; Object.hasOwn(definitions, name); n++) name = `${base}-${n}`;
    return name;
}

/** A union member's tag (`kind`, `type` or `status`), so `AgentEvent.message_delta` keeps its name
 * when another variant is added before it. */
function variant(type, at) {
    for (const key of ['kind', 'type', 'status']) {
        const property = type.getProperty?.(key);
        if (!property) continue;
        const value = checker.getTypeOfSymbolAtLocation(property, property.valueDeclaration ?? at);
        if (value.flags & ts.TypeFlags.StringLiteral) return value.value;
    }
    return null;
}

function schema(type, at, hint, ancestors = new Set()) {
    if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) return {};
    if (type.flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Void)) return { type: 'null' };
    if (type.flags & ts.TypeFlags.Never) return false;
    if (type.flags & ts.TypeFlags.Null) return { type: 'null' };
    if (type.flags & ts.TypeFlags.StringLiteral) return { const: type.value };
    if (type.flags & ts.TypeFlags.NumberLiteral) return { const: type.value };
    if (type.flags & ts.TypeFlags.BooleanLiteral)
        return { const: type.intrinsicName === 'true' };
    if (type.flags & ts.TypeFlags.StringLike) return { type: 'string' };
    if (type.flags & ts.TypeFlags.NumberLike) return { type: 'number' };
    if (type.flags & ts.TypeFlags.BooleanLike) return { type: 'boolean' };
    if (type.isUnion()) {
        const members = type.types.filter((t) => !(t.flags & ts.TypeFlags.Undefined));
        const alias = type.aliasSymbol?.name;
        const base = alias && !GENERIC.has(alias) ? alias : hint;
        return {
            anyOf: members.map((t, i) => schema(t, at, `${base}.${variant(t, at) ?? i}`, ancestors)),
        };
    }
    if (checker.isArrayType(type))
        return {
            type: 'array',
            items: schema(checker.getTypeArguments(type)[0], at, `${hint}.items`, ancestors),
        };
    if (checker.isTupleType(type))
        return {
            type: 'array',
            prefixItems: checker
                .getTypeArguments(type)
                .map((t, i) => schema(t, at, `${hint}.${i}`, ancestors)),
        };
    if (type.flags & ts.TypeFlags.Object || type.isIntersection()) {
        if (named.has(type)) return { $ref: `#/$defs/${named.get(type)}` };
        if (ancestors.has(type))
            throw Error(`Unresolved recursive type ${checker.typeToString(type)}`);
        const name = defName(type, hint);
        named.set(type, name);
        definitions[name] = {};
        const branch = new Set([...ancestors, type]);
        const properties = {},
            required = [];
        for (const property of checker.getPropertiesOfType(type)) {
            const value = checker.getTypeOfSymbolAtLocation(
                property,
                property.valueDeclaration ?? at,
            );
            const optional =
                property.flags & ts.SymbolFlags.Optional ||
                (value.isUnion() &&
                    value.types.some((t) => t.flags & ts.TypeFlags.Undefined));
            properties[property.name] = schema(value, at, `${name}.${property.name}`, branch);
            if (!optional) required.push(property.name);
        }
        const index = checker.getIndexTypeOfType(type, ts.IndexKind.String);
        definitions[name] = {
            type: 'object',
            properties,
            required,
            additionalProperties: index ? schema(index, at, `${name}.values`, branch) : false,
        };
        return { $ref: `#/$defs/${name}` };
    }
    throw Error(`Unsupported public type ${checker.typeToString(type)}`);
}

const operations = {};
function visit(node) {
    if (
        ts.isCallExpression(node) &&
        node.expression.getText() === 'invoke' &&
        ts.isStringLiteral(node.arguments[0])
    ) {
        const id = node.arguments[0].text;
        if (publicOperations.has(id)) {
            const args = node.arguments[1];
            const result = node.typeArguments?.[0];
            operations[id] = {
                arguments: args
                    ? schema(checker.getTypeAtLocation(args), args, `${id}.arguments`)
                    : { type: 'object', properties: {}, additionalProperties: false },
                result: result
                    ? schema(checker.getTypeFromTypeNode(result), result, `${id}.result`)
                    : {},
            };
        }
    }
    ts.forEachChild(node, visit);
}
visit(program.getSourceFile('src/lib/ipc/commands.ts'));

const source = program.getSourceFile('src/lib/extensions.ts');
const ui = source.statements.find(
    (node) => ts.isInterfaceDeclaration(node) && node.name.text === 'WorkbenchOperations',
);
if (!ui) throw Error('Workbench operation contracts are missing');
for (const member of ui.members) {
    const id = member.name.text;
    if (!publicOperations.has(id)) throw Error(`Classify ${id} in the public catalog`);
    const type = checker.getTypeAtLocation(member);
    const property = (key) =>
        checker.getTypeOfSymbolAtLocation(type.getProperty(key), member);
    operations[id] = {
        arguments: schema(property('arguments'), member, `${id}.arguments`),
        result: schema(property('result'), member, `${id}.result`),
    };
}
for (const id of publicOperations)
    if (!operations[id]) throw Error(`Missing public signature for ${id}`);
const result =
    JSON.stringify(
        {
            $schema: 'https://json-schema.org/draft/2020-12/schema',
            api: 2,
            operations,
            $defs: definitions,
        },
        null,
        2,
    ) + '\n';
const path = 'crates/wtm-plugins/contracts/operations.json';
if (process.argv.includes('--check')) {
    if (!fs.existsSync(path) || fs.readFileSync(path, 'utf8') !== result)
        throw Error(
            'Plugin operation schemas changed. Run bun scripts/plugin-contracts.mjs and review the generated contract.',
        );
} else fs.writeFileSync(path, result);
