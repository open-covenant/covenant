FROM node:22-slim AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=optional --ignore-scripts --no-audit --no-fund
COPY tsconfig.json ./
COPY src ./src
RUN npm run build

FROM node:22-slim
ENV NODE_ENV=production
ENV PORT=8930
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --omit=optional --ignore-scripts --no-audit --no-fund && npm cache clean --force
COPY --from=build /app/dist ./dist
USER node
EXPOSE 8930
CMD ["node", "dist/server.js"]
