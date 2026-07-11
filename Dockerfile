FROM alpine:3.20
RUN apk add --no-cache ca-certificates libgcc libstdc++
COPY ryuzi /usr/local/bin/ryuzi
ENTRYPOINT ["ryuzi"]
CMD ["start"]
