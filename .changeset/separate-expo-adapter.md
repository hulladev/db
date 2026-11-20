---
"@hulla/db-expo": minor
"@hulla/db-sqlite": minor
---

Move Expo support to `@hulla/db-expo`. Import `sqlite` from the new package and
pass the existing database filename to open a dedicated native connection with
secure UUID generation. If you provide a custom connection, import
`expoSQLiteDriver` from `@hulla/db-expo` and keep your existing `open` callback.
Install `expo-sqlite` and `expo-crypto` through Expo CLI for your app's SDK version.
The base SQLite and PostgreSQL packages have no Expo dependencies.
