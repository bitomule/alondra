import json
import sys

language, destination, *sources = sys.argv[1:]


def escaped(text):
    return text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


translations = {}
for source in sources:
    with open(source, encoding="utf-8") as handle:
        for key, entry in json.load(handle)["strings"].items():
            unit = entry.get("localizations", {}).get(language, {}).get("stringUnit")
            if unit:
                translations[key] = unit["value"]
lines = ['"%s" = "%s";' % (escaped(key), escaped(value)) for key, value in sorted(translations.items())]
with open(destination, "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines) + "\n")
