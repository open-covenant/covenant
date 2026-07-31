FROM node:22-slim
WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev=false
COPY tsconfig.json ./
COPY src ./src
RUN npm run build
ENV PORT=8930
EXPOSE 8930
CMD ["node", "dist/server.js"]
