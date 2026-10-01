import AlondraViews
import AlondraViewsDependency
import Foundation

/// Bundled, network-free Views documents for Demo. The fixture documents use the same
/// protocol DTOs as the real relay so the shipping Views UI runs unchanged.
enum DemoViews {
    private static let homeIOS = #"""
{
  "protocol": 1,
  "data_epoch": "test-epoch",
  "view": "home",
  "key": "home",
  "client": "ios",
  "version": "vfda4b38e2ecd5d09",
  "cursor": "cursor-1",
  "status": {
    "version": "v2d6ccffc4668392a",
    "summary": "1 question · 3 to read · 0 nodes working",
    "badge": 4,
    "counts": [
      {
        "id": "inbox",
        "label": "Inbox",
        "value": 4
      },
      {
        "id": "tasks",
        "label": "Tasks",
        "value": 1
      },
      {
        "id": "agents",
        "label": "Agents",
        "value": 0
      },
      {
        "id": "working",
        "label": "working",
        "value": 0
      },
      {
        "id": "waiting",
        "label": "waiting",
        "value": 0
      }
    ],
    "gauges": [],
    "orchestrator": {
      "text": "idle",
      "tone": "neutral"
    }
  },
  "sections": [
    {
      "id": "queue",
      "title": "Waiting for You",
      "count": 4,
      "collapsed": false,
      "items": [
        {
          "type": "card",
          "id": "i-1",
          "link": {
            "kind": "view",
            "key": "card:i-1"
          },
          "icon": "question",
          "eyebrow": {
            "text": "Question",
            "tone": "accent"
          },
          "title": "Build a feature",
          "context": [
            {
              "icon": "node",
              "text": "Node One",
              "link": {
                "kind": "view",
                "key": "node:a-1"
              }
            },
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "body": {
            "text": "Body 0",
            "truncated": false,
            "commentable": false
          },
          "actions": [
            {
              "id": "ans:i-1@1",
              "label": "Answer",
              "style": "primary",
              "presentation": "options",
              "input": {
                "kind": "none"
              },
              "attachments": true,
              "enabled": true,
              "stamp": "Answered"
            },
            {
              "id": "ctx:i-1@1",
              "label": "Need more context",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "What is missing?",
                "required": false,
                "multiline": true
              },
              "attachments": false,
              "enabled": true
            }
          ]
        },
        {
          "type": "card",
          "id": "i-2",
          "link": {
            "kind": "view",
            "key": "card:i-2"
          },
          "icon": "notice",
          "eyebrow": {
            "text": "Notice",
            "tone": "accent"
          },
          "title": "Build a feature",
          "context": [
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "body": {
            "text": "Body 1",
            "truncated": false,
            "commentable": false
          },
          "actions": [
            {
              "id": "read:i-2@1",
              "label": "Mark as read",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "none"
              },
              "attachments": false,
              "enabled": true,
              "stamp": "Read",
              "key": "x"
            },
            {
              "id": "reply:i-2@1",
              "label": "Reply",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "Write a reply",
                "required": true,
                "multiline": true
              },
              "attachments": true,
              "enabled": true
            }
          ]
        }
      ],
      "card_ids": [
        "i-1",
        "i-2",
        "i-3",
        "pr-1"
      ]
    }
  ]
}
"""#

    private static let homeMac = #"""
{
  "protocol": 1,
  "data_epoch": "test-epoch",
  "view": "home",
  "key": "home",
  "client": "tui",
  "version": "vb2c2a2a8c5f27ce6",
  "cursor": "cursor-1",
  "status": {
    "version": "v2d6ccffc4668392a",
    "summary": "1 question · 3 to read · 0 nodes working",
    "badge": 4,
    "counts": [
      {
        "id": "inbox",
        "label": "Inbox",
        "value": 4
      },
      {
        "id": "tasks",
        "label": "Tasks",
        "value": 1
      },
      {
        "id": "agents",
        "label": "Agents",
        "value": 0
      },
      {
        "id": "working",
        "label": "working",
        "value": 0
      },
      {
        "id": "waiting",
        "label": "waiting",
        "value": 0
      }
    ],
    "gauges": [],
    "orchestrator": {
      "text": "idle",
      "tone": "neutral"
    }
  },
  "sections": [
    {
      "id": "waiting",
      "title": "Waiting for You",
      "collapsed": false,
      "items": [
        {
          "type": "row",
          "id": "i-1",
          "link": {
            "kind": "view",
            "key": "card:i-1"
          },
          "icon": "question",
          "title": "Build a feature",
          "depth": 0,
          "struck": false
        },
        {
          "type": "row",
          "id": "i-3",
          "link": {
            "kind": "view",
            "key": "card:i-3"
          },
          "icon": "delivery",
          "title": "Build a feature",
          "depth": 0,
          "struck": false
        }
      ]
    },
    {
      "id": "pull_requests",
      "title": "Pull requests",
      "collapsed": false,
      "items": [
        {
          "type": "row",
          "id": "pr-1",
          "link": {
            "kind": "view",
            "key": "card:pr-1"
          },
          "icon": "pull_request",
          "title": "example/repo #12",
          "depth": 0,
          "struck": false,
          "children": [
            {
              "id": "i-5",
              "link": {
                "kind": "view",
                "key": "card:i-5"
              },
              "icon": "question",
              "title": "Build a feature",
              "depth": 1,
              "struck": false
            },
            {
              "id": "i-6",
              "link": {
                "kind": "view",
                "key": "card:i-6"
              },
              "icon": "notice",
              "title": "Build a feature",
              "depth": 1,
              "struck": false
            }
          ]
        }
      ]
    },
    {
      "id": "to_read",
      "title": "To Read",
      "collapsed": false,
      "items": [
        {
          "type": "row",
          "id": "i-2",
          "link": {
            "kind": "view",
            "key": "card:i-2"
          },
          "icon": "notice",
          "title": "Build a feature",
          "depth": 0,
          "struck": false
        }
      ]
    },
    {
      "id": "detail",
      "collapsed": false,
      "items": [
        {
          "type": "card",
          "id": "i-1",
          "link": {
            "kind": "view",
            "key": "card:i-1"
          },
          "icon": "question",
          "eyebrow": {
            "text": "Question",
            "tone": "accent"
          },
          "title": "Build a feature",
          "context": [
            {
              "icon": "node",
              "text": "Node One",
              "link": {
                "kind": "view",
                "key": "node:a-1"
              }
            },
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "body": {
            "text": "Body 0",
            "truncated": false,
            "commentable": false
          },
          "actions": [
            {
              "id": "ans:i-1@1",
              "label": "Answer",
              "style": "primary",
              "presentation": "options",
              "input": {
                "kind": "none"
              },
              "attachments": true,
              "enabled": true,
              "stamp": "Answered"
            },
            {
              "id": "ctx:i-1@1",
              "label": "Need more context",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "What is missing?",
                "required": false,
                "multiline": true
              },
              "attachments": false,
              "enabled": true,
              "key": "c"
            },
            {
              "id": "",
              "label": "Attach to node",
              "style": "plain",
              "presentation": "button",
              "input": {
                "kind": "none"
              },
              "attachments": false,
              "enabled": true,
              "key": "a",
              "link": {
                "kind": "local",
                "local": {
                  "kind": "attach",
                  "session": "a-1"
                }
              }
            }
          ]
        }
      ]
    }
  ]
}
"""#

    private static let questionCard = #"""
{
  "protocol": 1,
  "data_epoch": "test-epoch",
  "view": "card",
  "key": "card:i-1",
  "client": "ios",
  "version": "vbaf44f88529a6966",
  "cursor": "cursor-1",
  "title": "Build a feature",
  "status": {
    "version": "v2d6ccffc4668392a",
    "summary": "1 question · 3 to read · 0 nodes working",
    "badge": 4,
    "counts": [
      {
        "id": "inbox",
        "label": "Inbox",
        "value": 4
      },
      {
        "id": "tasks",
        "label": "Tasks",
        "value": 1
      },
      {
        "id": "agents",
        "label": "Agents",
        "value": 0
      },
      {
        "id": "working",
        "label": "working",
        "value": 0
      },
      {
        "id": "waiting",
        "label": "waiting",
        "value": 0
      }
    ],
    "gauges": [],
    "orchestrator": {
      "text": "idle",
      "tone": "neutral"
    }
  },
  "sections": [
    {
      "id": "card",
      "collapsed": false,
      "items": [
        {
          "type": "card",
          "id": "i-1",
          "link": {
            "kind": "view",
            "key": "card:i-1"
          },
          "icon": "question",
          "eyebrow": {
            "text": "Question",
            "tone": "accent"
          },
          "title": "Build a feature",
          "context": [
            {
              "icon": "node",
              "text": "Node One",
              "link": {
                "kind": "view",
                "key": "node:a-1"
              }
            },
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "body": {
            "text": "Body 0",
            "truncated": false,
            "commentable": false
          },
          "actions": [
            {
              "id": "ans:i-1@1",
              "label": "Answer",
              "style": "primary",
              "presentation": "options",
              "input": {
                "kind": "none"
              },
              "attachments": true,
              "enabled": true,
              "stamp": "Answered"
            },
            {
              "id": "ctx:i-1@1",
              "label": "Need more context",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "What is missing?",
                "required": false,
                "multiline": true
              },
              "attachments": false,
              "enabled": true
            }
          ]
        }
      ]
    }
  ]
}
"""#

    private static let deliveryCard = #"""
{
  "protocol": 1,
  "data_epoch": "test-epoch",
  "view": "card",
  "key": "card:i-3",
  "client": "ios",
  "version": "vf7955a142d6a051b",
  "cursor": "cursor-1",
  "title": "Build a feature",
  "status": {
    "version": "v2d6ccffc4668392a",
    "summary": "1 question · 3 to read · 0 nodes working",
    "badge": 4,
    "counts": [
      {
        "id": "inbox",
        "label": "Inbox",
        "value": 4
      },
      {
        "id": "tasks",
        "label": "Tasks",
        "value": 1
      },
      {
        "id": "agents",
        "label": "Agents",
        "value": 0
      },
      {
        "id": "working",
        "label": "working",
        "value": 0
      },
      {
        "id": "waiting",
        "label": "waiting",
        "value": 0
      }
    ],
    "gauges": [],
    "orchestrator": {
      "text": "idle",
      "tone": "neutral"
    }
  },
  "sections": [
    {
      "id": "card",
      "collapsed": false,
      "items": [
        {
          "type": "card",
          "id": "i-3",
          "link": {
            "kind": "view",
            "key": "card:i-3"
          },
          "icon": "delivery",
          "eyebrow": {
            "text": "Finished",
            "tone": "accent"
          },
          "title": "Build a feature",
          "context": [
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "body": {
            "text": "Body 2",
            "truncated": false,
            "commentable": false
          },
          "actions": [
            {
              "id": "accept:i-3@1",
              "label": "Accept",
              "style": "primary",
              "presentation": "button",
              "input": {
                "kind": "none"
              },
              "attachments": false,
              "enabled": false,
              "disabled_reason": "This card has changed",
              "stamp": "Accepted",
              "key": "v"
            },
            {
              "id": "changes:i-3@1",
              "label": "Request changes",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "Describe the changes",
                "required": true,
                "multiline": true
              },
              "attachments": true,
              "comments": {
                "kind": "blocks"
              },
              "enabled": false,
              "disabled_reason": "This card has changed",
              "stamp": "Changes requested",
              "key": "r"
            }
          ]
        }
      ]
    }
  ]
}
"""#

    private static let pullRequestCard = #"""
{
  "protocol": 1,
  "data_epoch": "test-epoch",
  "view": "card",
  "key": "card:pr-1",
  "client": "ios",
  "version": "v65d769f5bae723df",
  "cursor": "cursor-1",
  "title": "example/repo #12",
  "status": {
    "version": "v2d6ccffc4668392a",
    "summary": "1 question · 3 to read · 0 nodes working",
    "badge": 4,
    "counts": [
      {
        "id": "inbox",
        "label": "Inbox",
        "value": 4
      },
      {
        "id": "tasks",
        "label": "Tasks",
        "value": 1
      },
      {
        "id": "agents",
        "label": "Agents",
        "value": 0
      },
      {
        "id": "working",
        "label": "working",
        "value": 0
      },
      {
        "id": "waiting",
        "label": "waiting",
        "value": 0
      }
    ],
    "gauges": [],
    "orchestrator": {
      "text": "idle",
      "tone": "neutral"
    }
  },
  "sections": [
    {
      "id": "card",
      "collapsed": false,
      "items": [
        {
          "type": "card",
          "id": "pr-1",
          "link": {
            "kind": "view",
            "key": "card:pr-1"
          },
          "icon": "pull_request",
          "eyebrow": {
            "text": "Pull request",
            "tone": "accent"
          },
          "title": "example/repo #12",
          "context": [
            {
              "icon": "task",
              "text": "Build a feature",
              "link": {
                "kind": "view",
                "key": "task:t-1"
              }
            },
            {
              "icon": "mission",
              "text": "Mission · Build a feature",
              "link": {
                "kind": "view",
                "key": "request:r-1"
              }
            }
          ],
          "detail": "included",
          "children": [
            {
              "id": "i-5",
              "link": {
                "kind": "view",
                "key": "card:i-5"
              },
              "icon": "question",
              "eyebrow": {
                "text": "Question",
                "tone": "accent"
              },
              "title": "Build a feature",
              "context": [
                {
                  "icon": "task",
                  "text": "Build a feature",
                  "link": {
                    "kind": "view",
                    "key": "task:t-1"
                  }
                },
                {
                  "icon": "mission",
                  "text": "Mission · Build a feature",
                  "link": {
                    "kind": "view",
                    "key": "request:r-1"
                  }
                }
              ],
              "detail": "included",
              "body": {
                "text": "Review the PR",
                "truncated": false,
                "commentable": false
              },
              "actions": [
                {
                  "id": "ans:i-5@1",
                  "label": "Answer",
                  "style": "primary",
                  "presentation": "options",
                  "input": {
                    "kind": "none"
                  },
                  "attachments": true,
                  "enabled": true,
                  "stamp": "Answered"
                },
                {
                  "id": "ctx:i-5@1",
                  "label": "Need more context",
                  "style": "secondary",
                  "presentation": "button",
                  "input": {
                    "kind": "text",
                    "placeholder": "What is missing?",
                    "required": false,
                    "multiline": true
                  },
                  "attachments": false,
                  "enabled": true
                }
              ]
            },
            {
              "id": "i-6",
              "link": {
                "kind": "view",
                "key": "card:i-6"
              },
              "icon": "notice",
              "eyebrow": {
                "text": "Notice",
                "tone": "accent"
              },
              "title": "Build a feature",
              "context": [
                {
                  "icon": "node",
                  "text": "Node One",
                  "link": {
                    "kind": "view",
                    "key": "node:a-1"
                  }
                },
                {
                  "icon": "task",
                  "text": "Build a feature",
                  "link": {
                    "kind": "view",
                    "key": "task:t-1"
                  }
                },
                {
                  "icon": "mission",
                  "text": "Mission · Build a feature",
                  "link": {
                    "kind": "view",
                    "key": "request:r-1"
                  }
                }
              ],
              "detail": "included",
              "body": {
                "text": "The PR checks passed",
                "truncated": false,
                "commentable": false
              },
              "actions": [
                {
                  "id": "read:i-6@1",
                  "label": "Mark as read",
                  "style": "secondary",
                  "presentation": "button",
                  "input": {
                    "kind": "none"
                  },
                  "attachments": false,
                  "enabled": true,
                  "stamp": "Read",
                  "key": "x"
                },
                {
                  "id": "reply:i-6@1",
                  "label": "Reply",
                  "style": "secondary",
                  "presentation": "button",
                  "input": {
                    "kind": "text",
                    "placeholder": "Write a reply",
                    "required": true,
                    "multiline": true
                  },
                  "attachments": true,
                  "enabled": true
                }
              ]
            }
          ],
          "facts": {
            "items": [
              {
                "label": "Repository",
                "value": "example/repo #12"
              },
              {
                "label": "Base",
                "value": "main"
              }
            ]
          },
          "actions": [
            {
              "id": "ask_pr:pr-1@0",
              "label": "Ask about the PR",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "Your question",
                "required": true,
                "multiline": true
              },
              "attachments": true,
              "enabled": true
            },
            {
              "id": "changes:pr-1@0~abcdef12",
              "label": "Request changes",
              "style": "secondary",
              "presentation": "button",
              "input": {
                "kind": "text",
                "placeholder": "Describe the changes",
                "required": false,
                "multiline": true
              },
              "attachments": true,
              "comments": {
                "kind": "diff_lines"
              },
              "enabled": true,
              "link": {
                "kind": "view",
                "key": "pull_request:pr-1"
              }
            }
          ]
        }
      ]
    }
  ]
}
"""#


    static func client() -> ViewsClient {
        ViewsClient(
            load: { key, known in try load(key.rawValue, known: known) },
            open: { _ in },
            openScreen: { _, _ in },
            closeScreen: { _ in },
            feed: { AsyncStream { $0.finish() } },
            perform: { _ in try response() },
            fetchContent: { reference in
                if reference.kind == .diff {
                    return """
                    diff --git a/Welcome.swift b/Welcome.swift
                    --- a/Welcome.swift
                    +++ b/Welcome.swift
                    @@ -1 +1 @@
                    -let greeting = "Welcome to the dashboard"
                    +let greeting = "What needs your attention?"
                    """
                }
                return "# Sample document\n\nThis document exists only in the demo."
            },
            fetchAttachment: { _ in Data("Demo attachment".utf8) }
        )
    }

    private static func load(_ rawKey: String, known: String?) throws -> ViewLoad {
        let template: String
        if rawKey == "home" { template = homeIOS }
        else if rawKey == "sidebar" { template = homeMac }
        else if rawKey == "card:i-1" { template = questionCard }
        else if rawKey == "card:i-3" { template = deliveryCard }
        else if rawKey == "card:pr-1" { template = pullRequestCard }
        else if rawKey == "card:i-2" { template = questionCard.replacingOccurrences(of: "i-1", with: "i-2") }
        else if rawKey == "tasks" || rawKey.hasPrefix("tasks?") { template = screen("Tasks", id: "tasks") }
        else if rawKey == "fleet" || rawKey.hasPrefix("fleet?") { template = screen("Fleet", id: "fleet") }
        else if rawKey == "machine" { template = screen("Machine", id: "machine") }
        else if rawKey.hasPrefix("node:") { template = screen("Node One", id: "node") }
        else if rawKey.hasPrefix("task:") { template = screen("Build a feature", id: "task") }
        else if rawKey.hasPrefix("request:") { template = screen("Build a feature", id: "request") }
        else if rawKey.hasPrefix("pull_request:") { template = try pullRequestDetail() }
        else if rawKey.hasPrefix("card:") {
            let id = String(rawKey.dropFirst("card:".count))
            template = questionCard.replacingOccurrences(of: "i-1", with: id)
        } else if rawKey == "new_request" || rawKey.hasPrefix("new_request?") {
            template = try newRequest()
        } else {
            template = homeMac
        }

        let data = Data(template.utf8)
        var object = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        if rawKey == "home", var sections = object["sections"] as? [[String: Any]], !sections.isEmpty {
            sections[0]["items"] = [] // The current home contract loads cards through card_ids.
            object["sections"] = sections
        }
        object["key"] = rawKey
        object["view"] = rawKey.split(separator: "?", maxSplits: 1).first.map(String.init) ?? rawKey
        object["client"] = rawKey == "sidebar" ? "mac" : "ios"
        object["data_epoch"] = "demo-views"
        object["cursor"] = "demo-views-cursor"
        let stableVersion = rawKey.utf8.reduce(UInt64(14_695_981_039_346_656_037)) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }
        object["version"] = "demo-" + String(stableVersion, radix: 16)
        let document = try ViewsCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
        if known == document.version { return .unchanged(cursor: document.cursor) }
        return .document(document)
    }

    private static func newRequest() throws -> String {
        var object = try JSONSerialization.jsonObject(with: Data(homeIOS.utf8)) as! [String: Any]
        object["sections"] = [[
            "id": "new_request",
            "title": "New request",
            "collapsed": false,
            "items": [[
                "type": "form",
                "id": "new_request",
                "fields": [
                    ["id": "text", "label": "What would you like done?", "kind": ["kind": "multiline", "placeholder": "Describe the work"], "value": "", "required": true, "reload_on_change": false],
                    ["id": "attachments", "label": "Attachments", "kind": ["kind": "attachments"], "value": [], "required": false, "reload_on_change": false],
                    ["id": "project", "label": "Project", "kind": ["kind": "choice", "items": [["id": "Demo Garden", "label": "Demo Garden"]]], "value": "Demo Garden", "required": false, "reload_on_change": false]
                ],
                "submit": ["id": "new_request:submit@demo", "label": "Send Request", "style": "primary", "presentation": "button", "input": ["kind": "none"], "attachments": true, "enabled": true, "key": "n"]
            ]]
        ] as [String: Any]]
        return String(data: try JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }

    private static func pullRequestDetail() throws -> String {
        var document = try JSONSerialization.jsonObject(with: Data(screen("example/repo #12", id: "pull_request").utf8)) as! [String: Any]
        let cardView = try JSONSerialization.jsonObject(with: Data(pullRequestCard.utf8)) as! [String: Any]
        let card = ((cardView["sections"] as? [[String: Any]])?.first?["items"] as? [[String: Any]])?.first ?? [:]
        let detailActions = (card["actions"] as? [[String: Any]] ?? []).map { action in
            var action = action
            action.removeValue(forKey: "link") // A card navigates here; actions in the detail execute here.
            return action
        }
        document["sections"] = [
            ["id": "summary", "title": "example/repo #12", "collapsed": false,
             "items": [["type": "facts", "items": [["label": "Repository", "value": "example/repo"], ["label": "Base", "value": "main"]]]]],
            ["id": "actions", "collapsed": false,
             "items": [["type": "actions", "actions": detailActions]]],
            ["id": "diff", "title": "Diff", "collapsed": false,
             "items": [["type": "content_ref", "kind": "diff", "title": "Diff", "size": 0,
                         "digest": "demo-head-1", "fetch": "/pull-requests/pr-1/diff", "commentable": true]]]
        ] as [[String: Any]]
        return String(data: try JSONSerialization.data(withJSONObject: document), encoding: .utf8)!
    }

    private static func screen(_ title: String, id: String) -> String {
        let items: String
        switch id {
        case "tasks":
            items = #"{"type":"row","id":"t-1","link":{"kind":"view","key":"task:t-1"},"icon":"task","title":"Build a feature","subtitle":"Demo Garden","depth":0,"struck":false},{"type":"row","id":"r-1","link":{"kind":"view","key":"request:r-1"},"icon":"request","title":"Welcome experience","depth":0,"struck":false}"#
        case "fleet":
            items = #"{"type":"row","id":"a-1","link":{"kind":"view","key":"node:a-1"},"icon":"node","title":"Node One","subtitle":"Working on a sample task","depth":0,"struck":false},{"type":"row","id":"machine","link":{"kind":"view","key":"machine"},"icon":"machine","title":"Demo Mac","depth":0,"struck":false}"#
        default:
            items = #"{"type":"facts","items":[{"label":"Workspace","value":"Demo Garden"},{"label":"Source","value":"Sample data"}]}"#
        }
        return #"{"protocol":1,"data_epoch":"demo-views","view":"\#(id)","key":"\#(id)","client":"ios","version":"demo-1","cursor":"demo-views-cursor","status":{"version":"demo-1","summary":"Sample data","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}},"sections":[{"id":"\#(id)","title":"\#(title)","collapsed":false,"items":[\#(items)]}]}"#
    }

    private static func response() throws -> ActionOutcome {
        try ViewsCodec.decode(ActionOutcome.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved in the demo","revision":2}"#.utf8))
    }
}
