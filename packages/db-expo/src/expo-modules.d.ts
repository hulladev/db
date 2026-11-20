// Expo owns the native peer versions. The adapter uses only these exports.
declare module "expo-crypto" {
  export function getRandomValues<T extends Uint8Array>(array: T): T;
}

declare module "expo-sqlite" {
  export function openDatabaseAsync(
    filename: string,
    options: { useNewConnection: boolean },
  ): Promise<import("./index.ts").ExpoSQLiteConnection>;
}
