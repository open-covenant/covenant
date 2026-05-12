// Production `pnpm build` runs strict TypeScript and refuses side-effect
// CSS imports without a declaration. `next-env.d.ts` doesn't cover this
// case in Next 15 App Router, so we declare CSS as an ambient module.
declare module "*.css";
