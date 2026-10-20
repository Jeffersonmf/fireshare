# Static Linux server image: musl build, nothing else in the final layer.
FROM rust:1.97-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM scratch
COPY --from=build /src/target/release/fireshare /fireshare
EXPOSE 7878
VOLUME ["/shared"]
ENV FIRESHARE_DIR=/shared
ENTRYPOINT ["/fireshare"]
CMD ["serve"]
