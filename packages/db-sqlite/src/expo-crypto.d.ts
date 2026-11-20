/** The optional Expo peer exposes a secure native random-value filler. */
declare module "expo-crypto" {
  export function getRandomValues<T extends Uint8Array>(array: T): T;
}
