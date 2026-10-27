import AlondraContract
import XCTest

final class AccountCommandTests: XCTestCase {
    func testProviderCannotBeChangedByALinkCommandOrMismatchedArguments() {
        XCTAssertNil(AccountCommand.command(["accounts", "login", "--provider", "claude", "work"], provider: .codex, adding: false))
        XCTAssertNil(AccountCommand.command(["accounts", "remove", "--provider", "codex", "work"], provider: .codex, adding: false))
        XCTAssertNil(AccountCommand.command(["accounts", "login", "--provider", "codex", "--all"], provider: .codex, adding: false))
        XCTAssertNil(AccountCommand.command(["accounts", "login", "--provider", "codex", "work", "extra"], provider: .codex, adding: false))
        XCTAssertNil(AccountCommand.command(["accounts", "login", "--provider", "codex", "work\ncommand"], provider: .codex, adding: false))
    }

    func testAddingAnAccountQuotesEveryCharacterAndSeparatesItsNameFromOptions() {
        let command = AccountCommand.command(["accounts", "add", "--provider", "codex"], provider: .codex, adding: true, newName: "  --Pat's $(echo private)  ")
        XCTAssertEqual(command, #"'alondra' 'accounts' 'add' '--provider' 'codex' '--' '--Pat'\''s $(echo private)'"#)
        XCTAssertNil(AccountCommand.command(["accounts", "add", "--provider", "codex"], provider: .codex, adding: true, newName: "   "))
    }
}
