/**
 * The result grid's addressing and clipboard formats, kept out of the component.
 *
 * Nothing here touches the DOM, so the rules that decide what lands on the clipboard can be read in
 * one place — and with no JS test runner, being readable is most of what keeps them right.
 */

import type { QueryCell } from './ipc/types';

/**
 * A cell as the grid shows it. `row` counts the rows on screen, not the result's rows — once find
 * hides the rows that do not match, "the next row down" means the next one you can see.
 */
export interface CellAt {
  row: number;
  col: number;
}

/** A selection handed to someone else — an agent's composer — as a header and its rows. */
export interface SelectedCells {
  header: string[];
  rows: string[][];
}

/** The rectangle two corners span, inclusive at both ends. */
export interface Rect {
  top: number;
  left: number;
  bottom: number;
  right: number;
}

export function rectOf(a: CellAt, b: CellAt): Rect {
  return {
    top: Math.min(a.row, b.row),
    left: Math.min(a.col, b.col),
    bottom: Math.max(a.row, b.row),
    right: Math.max(a.col, b.col),
  };
}

export function contains(rect: Rect | null, row: number, col: number): boolean {
  return (
    rect !== null &&
    row >= rect.top &&
    row <= rect.bottom &&
    col >= rect.left &&
    col <= rect.right
  );
}

/**
 * What a cell copies as. NULL copies as nothing: pasted into a query or a spreadsheet, an empty
 * field is what NULL means, where the word would arrive as a four-letter string.
 */
export function cellText(cell: QueryCell): string {
  return cell.value ?? '';
}

/**
 * One delimited field, quoted only when it has to be: RFC 4180's rule, with the delimiter as a
 * parameter so TSV shares it. Spreadsheets accept the same quoting in a tab-separated paste.
 */
function field(value: string, delimiter: string): string {
  return value.includes(delimiter) || /["\r\n]/.test(value)
    ? `"${value.replaceAll('"', '""')}"`
    : value;
}

function delimited(rows: string[][], delimiter: string): string {
  return rows
    .map((row) => row.map((value) => field(value, delimiter)).join(delimiter))
    .join('\n');
}

/** `3,9\n4,10` — rows on lines, cells split by commas. */
export function toCsv(rows: string[][]): string {
  return delimited(rows, ',');
}

/** For a spreadsheet: tabs split cells, so a value with a comma needs no quoting. */
export function toTsv(rows: string[][]): string {
  return delimited(rows, '\t');
}

/**
 * Column names made unique for a JSON object's keys — `id`, `id_2` — because a join can return
 * two columns with one name, and an object holding both would silently keep only the second.
 */
function uniqueNames(names: string[]): string[] {
  const seen = new Map<string, number>();
  return names.map((name) => {
    const count = (seen.get(name) ?? 0) + 1;
    seen.set(name, count);
    return count === 1 ? name : `${name}_${count}`;
  });
}

/**
 * An array of objects. Every value is a string or null: the grid receives text, and guessing
 * which strings were numbers would turn a zip code of `02134` into `2134`.
 */
export function toJson(names: string[], rows: QueryCell[][]): string {
  const keys = uniqueNames(names);
  const objects = rows.map((row) =>
    Object.fromEntries(keys.map((key, i) => [key, row[i]?.value ?? null])),
  );
  return JSON.stringify(objects, null, 2);
}

/** Mirrors `quote_identifier` in `wtm-db`, so an identifier written here reads the same there. */
export function quoteIdentifier(name: string): string {
  return `"${name.replaceAll('"', '""')}"`;
}

/**
 * A value as an SQL literal. Always a quoted string, whatever the column's type: the grid never
 * learns Postgres types, and an untyped literal is coerced to the column's type on insert — `'1'`
 * into an integer, `'{"a":1}'` into jsonb — where a guess would sometimes be wrong.
 */
export function sqlLiteral(value: string | null): string {
  return value === null ? 'NULL' : `'${value.replaceAll("'", "''")}'`;
}

/** One statement per row, so a paste can be trimmed to the rows wanted by deleting lines. */
export function toInsert(
  schema: string,
  table: string,
  names: string[],
  rows: QueryCell[][],
): string {
  const target = `${quoteIdentifier(schema)}.${quoteIdentifier(table)}`;
  const columns = names.map(quoteIdentifier).join(', ');
  return rows
    .map(
      (row) =>
        `INSERT INTO ${target} (${columns}) VALUES (${row.map((cell) => sqlLiteral(cell.value)).join(', ')});`,
    )
    .join('\n');
}

/**
 * A value to read at length: JSON objects and arrays laid out, anything else as it came. Null when
 * there is nothing to lay out, so the viewer can say whether it is showing the value or a layout.
 */
export function prettyJson(value: string): string | null {
  const text = value.trim();
  if (!(text.startsWith('{') || text.startsWith('['))) return null;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return null;
  }
}

/** What the grid's sort toggle writes into ORDER BY. */
export function sortClause(column: string, direction: 'asc' | 'desc'): string {
  return `${quoteIdentifier(column)} ${direction === 'desc' ? 'DESC' : 'ASC'}`;
}

/**
 * The column and direction when ORDER BY is exactly what the toggle writes, else null — so the
 * header shows an arrow for its own sort and none for an ORDER BY typed by hand, which may name an
 * expression or several columns the arrow could not describe.
 */
export function sortOf(
  orderBy: string,
): { column: string; direction: 'asc' | 'desc' } | null {
  const match = /^"((?:[^"]|"")+)"\s+(ASC|DESC)$/i.exec(orderBy.trim());
  if (!match?.[1] || !match[2]) return null;
  return {
    column: match[1].replaceAll('""', '"'),
    direction: match[2].toLowerCase() === 'desc' ? 'desc' : 'asc',
  };
}

/** "Filter by this value": the condition that keeps rows whose column holds exactly this. */
export function equalsCondition(column: string, value: string | null): string {
  return value === null
    ? `${quoteIdentifier(column)} IS NULL`
    : `${quoteIdentifier(column)} = ${sqlLiteral(value)}`;
}
