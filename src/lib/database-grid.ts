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
