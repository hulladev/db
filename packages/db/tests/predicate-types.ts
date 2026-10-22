import type { WriteDatabase } from "../src/database.ts";
import {createTables} from "../src/fields.ts";
import {defineSchema,defineTable,v} from "../src/schema.ts";
const schema=defineSchema({users:defineTable({name:v.string(),age:v.int64({ mode: "bigint" }),enabled:v.boolean()}),other:defineTable({name:v.string()})});
declare const db:WriteDatabase<typeof schema>;
const {users,other}=createTables<typeof schema>({users:{"id":{},"name":{},"age":{},"enabled":{}},other:{"id":{},"name":{}}});
db.select(users).where(users.age,">",2n).where(({eq,and,gt})=>and(eq(users.name,"literal"),gt("age",1n))).orderBy(users.name);
// @ts-expect-error field value cannot widen to another column's value type
db.select(users).where("age","=","bad");
// @ts-expect-error field reference value cannot widen to string
db.select(users).where(users.age,"=","bad");
// @ts-expect-error callbacks preserve operand type
db.select(users).where(({eq})=>eq("age","bad"));
// @ts-expect-error callback references preserve operand type
db.select(users).where(({eq})=>eq(users.age,"bad"));
// @ts-expect-error other table field cannot scope predicate
db.select(users).where(other.name,"=","bad");
// @ts-expect-error other table cannot be an operand
db.select(users).where(users.name,"=",other.name);
// @ts-expect-error boolean ordering is not meaningful
db.select(users).where("enabled",">",true);
// @ts-expect-error text helper does not accept numeric fields
db.select(users).where(({contains})=>contains(users.age,"1"));
// @ts-expect-error callback boolean ranges cannot widen to another scalar type
db.select(users).where(({between})=>between("enabled",[1n,2n]));
// @ts-expect-error callback boolean reference ranges cannot widen either
db.select(users).where(({between})=>between(users.enabled,[1n,2n]));
db.insert(users,{name:"valid",age:1n,enabled:true});
// @ts-expect-error insert reference retains table row input types
db.insert(users,{name:"valid",age:"invalid",enabled:true});
// @ts-expect-error ordered values cannot be null
db.select(users).where(users.age,">",null);
// @ts-expect-error text operands are literals, not field references
db.select(users).where(users.name,"contains",users.name);
const jsonSchema=defineSchema({items:defineTable({settings:v.json({theme:v.string().optional(),tags:v.array(v.string().nullable()),$value:v.string()})})});
declare const jsonDb:WriteDatabase<typeof jsonSchema>;
const {items}=createTables<typeof jsonSchema>({items:{id:{},settings:{}}});
jsonDb.select(items).where("settings.theme","exists").where("settings.tags","contains",null).where(items.settings.$value,"=","value");
jsonDb.select(items).where(({gte,arrayLength})=>gte(arrayLength(items.settings.tags),3));
// @ts-expect-error array membership retains element type
jsonDb.select(items).where("settings.tags","contains",4);
// @ts-expect-error array membership options belong only to strings
jsonDb.select(items).where("settings.tags","contains","x",{caseSensitive:false});
// @ts-expect-error no implicit array index paths
jsonDb.select(items).where("settings.tags.0","=","x");
// @ts-expect-error callback array membership retains element type
jsonDb.select(items).where(({containsAny})=>containsAny(items.settings.tags,[4]));
