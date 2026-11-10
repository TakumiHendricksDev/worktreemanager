import wire from '../../crates/wtm-plugins/contracts/operations.json';

type Schema =
  | boolean
  | {
      $ref?: string;
      type?: string;
      const?: unknown;
      anyOf?: Schema[];
      properties?: Record<string, Schema>;
      required?: string[];
      additionalProperties?: Schema;
      items?: Schema;
      prefixItems?: Schema[];
    };
const definitions = wire.$defs as Record<string, Schema>;
const operations = wire.operations as Record<string, { arguments: Schema; result: Schema }>;

function matches(schema: Schema, value: unknown, depth: number): boolean {
  if (depth > 64) return false;
  if (typeof schema === 'boolean') return schema;
  if (schema.$ref) {
    const referred = definitions[schema.$ref.slice('#/$defs/'.length)];
    return referred !== undefined && matches(referred, value, depth + 1);
  }
  if ('const' in schema) return value === schema.const;
  if (schema.anyOf) return schema.anyOf.some((s) => matches(s, value, depth + 1));
  switch (schema.type) {
    case 'null':
      return value === null;
    case 'string':
      return typeof value === 'string';
    case 'number':
      return typeof value === 'number' && Number.isFinite(value);
    case 'boolean':
      return typeof value === 'boolean';
    case 'array':
      return (
        Array.isArray(value) &&
        value.every((v, i) =>
          matches(schema.prefixItems?.[i] ?? schema.items ?? true, v, depth + 1),
        )
      );
    case 'object': {
      if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
      const object = value as Record<string, unknown>;
      if (schema.required?.some((key) => object[key] === undefined)) return false;
      return Object.entries(object).every(
        ([key, v]) =>
          v === undefined ||
          matches(
            (schema.properties && Object.hasOwn(schema.properties, key)
              ? schema.properties[key]
              : schema.additionalProperties) ?? true,
            v,
            depth + 1,
          ),
      );
    }
    default:
      return true;
  }
}

function contract(operation: string) {
  return Object.hasOwn(operations, operation) ? operations[operation] : undefined;
}

/** Both plugin requests and hook transforms use the same published wire shapes as built-ins. */
export function validateExtensionArguments(operation: string, args: unknown): void {
  const published = contract(operation);
  if (!published || !matches(published.arguments, args, 0))
    throw Error(`Arguments do not match the published contract for ${operation}`);
}

/**
 * A replacement's answer, held to the result shape of the operation it stands in for.
 *
 * Its caller is built-in code typed against that shape, so an unchecked answer would surface as a
 * crash somewhere far from the plugin that caused it.
 */
export function validateExtensionResult(operation: string, result: unknown): void {
  const published = contract(operation);
  if (!published || !matches(published.result, result, 0))
    throw Error(
      `A replacement's result does not match the published contract for ${operation}`,
    );
}
