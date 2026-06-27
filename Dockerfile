FROM alpine:3.20
RUN apk add --no-cache ca-certificates libgcc libstdc++
COPY hr /usr/local/bin/hr
ENTRYPOINT ["hr"]
