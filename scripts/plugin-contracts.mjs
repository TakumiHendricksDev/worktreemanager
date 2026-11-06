// Derive the author-facing wire schemas from the typed entrypoints used by WTM itself.
// A stale generated contract fails verification rather than teaching agents an invented API.
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
let next = 0;

function schema(type, at, ancestors = new Set()) {
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
        return { anyOf: members.map((t) => schema(t, at, ancestors)) };
    }
    if (checker.isArrayType(type))
        return {
            type: 'array',
            items: schema(checker.getTypeArguments(type)[0], at, ancestors),
        };
    if (checker.isTupleType(type))
        return {
            type: 'array',
            prefixItems: checker
                .getTypeArguments(type)
                .map((t) => schema(t, at, ancestors)),
        };
    if (type.flags & ts.TypeFlags.Object || type.isIntersection()) {
        if (named.has(type)) return { $ref: `#/$defs/${named.get(type)}` };
        if (ancestors.has(type))
            throw Error(`Unresolved recursive type ${checker.typeToString(type)}`);
        const name = `t${++next}`;
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
            properties[property.name] = schema(value, at, branch);
            if (!optional) required.push(property.name);
        }
        const index = checker.getIndexTypeOfType(type, ts.IndexKind.String);
        definitions[name] = {
            type: 'object',
            properties,
            required,
            additionalProperties: index ? schema(index, at, branch) : false,
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
                    ? schema(checker.getTypeAtLocation(args), args)
                    : { type: 'object', properties: {}, additionalProperties: false },
                result: result ? schema(checker.getTypeFromTypeNode(result), result) : {},
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
        arguments: schema(property('arguments'), member),
        result: schema(property('result'), member),
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
