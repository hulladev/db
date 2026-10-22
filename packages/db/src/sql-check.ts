const fieldBrand = Symbol('SQL check field');
const checkBrand = Symbol('SQL check expression');

export interface CheckColumn { readonly [fieldBrand]: string }
export type CheckFields<K extends string> = { readonly [P in K]: CheckColumn };
export type CheckLiteral = string | number | bigint | boolean | null;
export type CheckOperand = { readonly field: string } | { readonly value: CheckLiteral };
export interface SqlCheck {
  readonly [checkBrand]: true;
  readonly parts: readonly string[];
  readonly operands: readonly CheckOperand[];
}
export interface CompiledCheck {
  readonly name: string;
  readonly parts: readonly string[];
  readonly operands: readonly CheckOperand[];
}

function literal(value: unknown): CheckLiteral {
  if (value === null || typeof value === 'boolean') return value;
  if (typeof value === 'string' && !/[\uD800-\uDFFF]/u.test(value) && !value.includes('\0')) return value;
  if (typeof value === 'number' && Number.isFinite(value)) return value === 0 ? 0 : value;
  if (typeof value === 'bigint' && value >= -(1n << 63n) && value < (1n << 63n)) return value;
  throw new TypeError('SQL check interpolations require a field, expression or finite scalar literal');
}

/** Trusted SQL expression text; interpolated fields and values are quoted separately. */
export function sql(parts: TemplateStringsArray, ...values: readonly (CheckColumn | SqlCheck | CheckLiteral)[]): SqlCheck {
  if (!Array.isArray(parts) || !Array.isArray(parts.raw) || parts.length !== values.length + 1)
    throw new TypeError('Use sql as a tagged template');
  const text: string[] = [parts[0]!];
  const operands: CheckOperand[] = [];
  for (let index = 0; index < values.length; index++) {
    const value = values[index];
    if (value !== null && typeof value === 'object' && checkBrand in value) {
      text[text.length - 1] += value.parts[0]!;
      value.operands.forEach((operand, part) => { operands.push(operand); text.push(value.parts[part + 1]!); });
      text[text.length - 1] += parts[index + 1]!;
    } else {
      operands.push(Object.freeze(value !== null && typeof value === 'object' && fieldBrand in value
        ? { field: value[fieldBrand] } : { value: literal(value) }));
      text.push(parts[index + 1]!);
    }
  }
  if (text.some(part => typeof part !== 'string' || /\0|;|--|\/\*|\*\//.test(part)))
    throw new TypeError('SQL checks accept one expression without statement separators or comments');
  return Object.freeze({ [checkBrand]: true as const, parts: Object.freeze(text), operands: Object.freeze(operands) });
}

export function compileChecks<const K extends string>(
  callback: ((fields: CheckFields<K>) => Readonly<Record<string, SqlCheck>>) | undefined,
  fields: readonly K[],
  _primaryKey: string,
): readonly CompiledCheck[] {
  if (callback === undefined) return Object.freeze([]);
  if (typeof callback !== 'function') throw new TypeError('Table checks must be a callback');
  const columns = Object.freeze(Object.fromEntries(fields.map(name => [name, Object.freeze({ [fieldBrand]: name })]))) as unknown as CheckFields<K>;
  const checks = callback(columns);
  if (!checks || typeof checks !== 'object' || ![Object.prototype, null].includes(Object.getPrototypeOf(checks)))
    throw new TypeError('Table checks must return a plain record of SQL expressions');
  return Object.freeze(Reflect.ownKeys(checks).map(name => {
    if (typeof name !== 'string' || !name || name.includes('\0') || /[\uD800-\uDFFF]/u.test(name))
      throw new TypeError('SQL checks require valid nonempty names');
    const property = Object.getOwnPropertyDescriptor(checks, name)!;
    if (!property.enumerable || !('value' in property) || !property.value || property.value[checkBrand] !== true)
      throw new TypeError('Table checks must contain SQL expressions without accessors');
    const expression = property.value as SqlCheck;
    if (expression.operands.some(operand => 'field' in operand && !(fields as readonly string[]).includes(operand.field)))
      throw new TypeError('SQL checks can only reference fields of their own table');
    if (!expression.parts.some(part => part.trim()) && !expression.operands.length)
      throw new TypeError('SQL checks cannot be empty');
    return Object.freeze({ name, parts: expression.parts, operands: expression.operands });
  }));
}
