FROM postgres:18.6-bookworm
RUN apt-get update && apt-get install --yes --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
COPY scripts/backup.py /usr/local/bin/hulladb-backup.py
ENTRYPOINT ["python3", "/usr/local/bin/hulladb-backup.py"]
