FROM node:22-bookworm-slim
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts
COPY tsconfig.json ./
COPY src ./src
COPY plugins ./plugins
COPY bin ./bin
RUN mkdir -p /data && chown -R node:node /data
USER node
EXPOSE 4291
ENTRYPOINT ["node", "bin/cito-mcp", "hub"]
