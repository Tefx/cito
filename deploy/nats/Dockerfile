FROM node:22-bookworm-slim
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts
COPY tsconfig.json ./
COPY src/shared ./src/shared
COPY src/auth-callout ./src/auth-callout
COPY bin/cito-auth-callout ./bin/cito-auth-callout
USER node
ENTRYPOINT ["node", "bin/cito-auth-callout"]
