import json
import plistlib
import re
import sys

language, destination, *sources = sys.argv[1:]

FORMAT_SPECIFIER = re.compile(r"%(?:\d+\$)?[-+ #0]*\d*(?:\.\d+)?((?:ll|l|hh|h|q|z|t|j)?[dioxXucsfeEgGaAp@])")


def escaped(text):
    return text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def plural_entry(key, forms):
    specifier = FORMAT_SPECIFIER.search(key)
    if not specifier:
        raise SystemExit("%r has plural variations but no format specifier to pick the form by" % key)
    rule = {"NSStringFormatSpecTypeKey": "NSStringPluralRuleType", "NSStringFormatValueTypeKey": specifier.group(1)}
    rule.update({form: variation["stringUnit"]["value"] for form, variation in forms.items()})
    return {"NSStringLocalizedFormatKey": "%#@value@", "value": rule}


translations = {}
plurals = {}
for source in sources:
    with open(source, encoding="utf-8") as handle:
        for key, entry in json.load(handle)["strings"].items():
            localization = entry.get("localizations", {}).get(language, {})
            unit = localization.get("stringUnit")
            if unit:
                translations[key] = unit["value"]
            forms = localization.get("variations", {}).get("plural")
            if forms:
                plurals[key] = plural_entry(key, forms)
lines = ['"%s" = "%s";' % (escaped(key), escaped(value)) for key, value in sorted(translations.items())]
with open(destination, "w", encoding="utf-8") as handle:
    handle.write("\n".join(lines) + "\n")
with open(destination.removesuffix(".strings") + ".stringsdict", "wb") as handle:
    plistlib.dump(plurals, handle, sort_keys=True)
