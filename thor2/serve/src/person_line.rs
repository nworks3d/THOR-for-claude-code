//! The one plain sentence for the PERSON that goes beside a hold or a refusal.
//!
//! A block's `reason` is written for the assistant, in THOR's own words, and
//! Claude Code shows it on the person's screen as well. Someone who has never
//! heard of any of this reads "rule", "falsifier" and "evaluation" and has no
//! idea whether something broke. So every block also carries a top-level
//! `systemMessage` - Claude Code's channel for the person - saying in ordinary
//! words what THOR asks the assistant to do, and whether anything is asked of
//! the person.
//!
//! Every kind of hold has its own true sentence, in the language recorded with
//! `install --set-language` (English when none is set). A hold that matches no
//! kind gets a neutral sentence that is true for any hold. A test pins every
//! kind to a sentence in every language.

use crate::{leftover_scripts, other_project};

/// Every kind of hold or refusal a person can see a sentence for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Setup,
    Order,
    Scripts,
    NoProject,
    CopyProof,
    TidyUp,
    Decision,
    NotesOutOfDate,
    Crowded,
    TurnViolation,
    ReplyLanguage,
    NeedsYourOk,
    ActionStopped,
    StoreUnwritable,
    Neutral,
}

/// Every kind, for the test that walks them.
pub const ALL: [Hold; 15] = [
    Hold::Setup,
    Hold::Order,
    Hold::Scripts,
    Hold::NoProject,
    Hold::CopyProof,
    Hold::TidyUp,
    Hold::Decision,
    Hold::NotesOutOfDate,
    Hold::Crowded,
    Hold::TurnViolation,
    Hold::ReplyLanguage,
    Hold::NeedsYourOk,
    Hold::ActionStopped,
    Hold::StoreUnwritable,
    Hold::Neutral,
];

/// The phrase of the assistant-facing text that says a note was stored onto a
/// place that is already full (`serve.rs`'s crowding hold).
pub const CROWDED_MARKER: &str = "onto a place that is already full";
/// The start of the assistant-facing text of a rule broken by a script.
pub const TURN_VIOLATION_MARKER: &str = "a rule is broken: a file now holds text";
/// The reply-language reminder's own words (`respond::rulebook_with_language`).
pub const REPLY_LANGUAGE_MARKER: &str = "but it reads as another language";
/// The first words of the hold that carries the memory-cannot-be-written line.
pub const STORE_UNWRITABLE_MARKER: &str = "memory file cannot be written";

/// Which hold a block is, chosen from the event and a few phrases of its
/// reason. When several holds ride in one block, the one that comes first in
/// the text wins; the evaluation ask, which rides along with many of them, only
/// counts when nothing more specific is there.
pub fn classify(event: &str, reason: &str) -> Hold {
    let lower = reason.to_lowercase();
    if event == "Stop" {
        let specific: [(String, Hold); 11] = [
            (other_project::HOLD_MARKER.to_lowercase(), Hold::NoProject),
            ("walked through setup".to_string(), Hold::Setup),
            ("[thor] order-earlier-work".to_string(), Hold::Order),
            (leftover_scripts::HOLD_MARKER.to_lowercase(), Hold::Scripts),
            ("copy-attestation".to_string(), Hold::CopyProof),
            ("durable decision".to_string(), Hold::Decision),
            ("could not prove themselves".to_string(), Hold::NotesOutOfDate),
            (CROWDED_MARKER.to_string(), Hold::Crowded),
            (TURN_VIOLATION_MARKER.to_string(), Hold::TurnViolation),
            (REPLY_LANGUAGE_MARKER.to_string(), Hold::ReplyLanguage),
            (STORE_UNWRITABLE_MARKER.to_string(), Hold::StoreUnwritable),
        ];
        let first = specific.iter().filter_map(|(marker, hold)| lower.find(marker.as_str()).map(|at| (at, *hold))).min_by_key(|(at, _)| *at);
        if let Some((_, hold)) = first {
            return hold;
        }
        if lower.contains("evaluation") {
            return Hold::TidyUp;
        }
        return Hold::Neutral;
    }
    if event == "PreToolUse" {
        return if lower.contains("needs the owner's explicit go") { Hold::NeedsYourOk } else { Hold::ActionStopped };
    }
    Hold::Neutral
}

/// The languages a sentence exists in.
pub const LANGUAGES: [&str; 5] = ["en", "nl", "de", "fr", "es"];

/// The sentence for `hold` in `language` (a code recorded with
/// `install --set-language`; anything else reads as English).
pub fn line(hold: Hold, language: Option<&str>) -> &'static str {
    let at = match language.map(str::to_lowercase).as_deref() {
        Some("nl") => 1,
        Some("de") => 2,
        Some("fr") => 3,
        Some("es") => 4,
        _ => 0,
    };
    let row: [&'static str; 5] = match hold {
        Hold::Setup => [
            "THOR (the memory your assistant uses) is finishing its setup. Your assistant welcomes you and asks one short question with a few choices: you can simply answer \"yes\". Nothing else is needed from you.",
            "THOR (het geheugen van je assistent) rondt de installatie af. Je assistent verwelkomt je en stelt een korte vraag met een paar keuzes: je kunt gewoon \"ja\" antwoorden. Verder hoef je niets te doen.",
            "THOR (das Gedächtnis Ihres Assistenten) schließt gerade die Einrichtung ab. Ihr Assistent begrüßt Sie und stellt eine kurze Frage mit ein paar Auswahlmöglichkeiten: Sie können einfach \"ja\" antworten. Mehr ist nicht nötig.",
            "THOR (la mémoire de votre assistant) termine son installation. Votre assistant vous accueille et pose une courte question avec quelques choix : vous pouvez simplement répondre \"oui\". Rien d'autre n'est nécessaire de votre part.",
            "THOR (la memoria de tu asistente) está terminando su configuración. Tu asistente te da la bienvenida y te hace una pregunta corta con algunas opciones: puedes responder simplemente \"sí\". No necesitas hacer nada más.",
        ],
        Hold::Order => [
            "THOR asks your assistant to finish one last setup step: putting your earlier work in order. If it needs a decision from you it asks one short question; otherwise nothing is needed from you.",
            "THOR vraagt je assistent een laatste stap van de installatie af te ronden: je eerdere werk op orde brengen. Heeft het een beslissing van je nodig, dan stelt het één korte vraag; anders hoef je niets te doen.",
            "THOR bittet Ihren Assistenten, einen letzten Einrichtungsschritt abzuschließen: Ihre frühere Arbeit zu ordnen. Braucht er eine Entscheidung von Ihnen, stellt er eine kurze Frage; sonst ist nichts nötig.",
            "THOR demande à votre assistant de terminer une dernière étape : mettre votre travail précédent en ordre. S'il a besoin d'une décision de votre part, il pose une courte question ; sinon, rien n'est nécessaire.",
            "THOR pide a tu asistente que termine un último paso: poner en orden tu trabajo anterior. Si necesita una decisión tuya, te hará una pregunta corta; si no, no necesitas hacer nada.",
        ],
        Hold::Scripts => [
            "Your assistant left a few small helper files outside your projects. THOR asks it to tidy them up: useful ones are kept in your tools folder, leftovers are deleted, and it asks you first about anything unclear. You do not need to do anything unless it asks you.",
            "Je assistent heeft een paar kleine hulpbestanden buiten je projecten laten staan. THOR vraagt het die op te ruimen: nuttige gaan naar je gereedschapsmap, de rest wordt verwijderd, en bij twijfel vraagt het eerst aan jou. Je hoeft niets te doen, tenzij het iets vraagt.",
            "Ihr Assistent hat ein paar kleine Hilfsdateien außerhalb Ihrer Projekte liegen lassen. THOR bittet ihn aufzuräumen: nützliche kommen in Ihren Werkzeugordner, der Rest wird gelöscht, und bei Unklarheit fragt er Sie zuerst. Sie müssen nichts tun, außer er fragt Sie.",
            "Votre assistant a laissé quelques petits fichiers d'aide en dehors de vos projets. THOR lui demande de ranger : les utiles vont dans votre dossier d'outils, le reste est supprimé, et il vous demande d'abord en cas de doute. Vous n'avez rien à faire, sauf s'il vous pose une question.",
            "Tu asistente dejó algunos archivos auxiliares pequeños fuera de tus proyectos. THOR le pide que los ordene: los útiles van a tu carpeta de herramientas, el resto se borra, y te pregunta antes si hay dudas. No necesitas hacer nada, salvo que te pregunte.",
        ],
        Hold::NoProject => [
            "This conversation has no project folder, so what is said here cannot be kept under a project. Your assistant proposes a folder and asks you one question: just answer yes or no. Nothing else is needed from you.",
            "Dit gesprek heeft geen projectmap, dus wat hier gezegd wordt kan niet bij een project bewaard worden. Je assistent stelt een map voor en stelt je één vraag: antwoord gewoon ja of nee. Verder hoef je niets te doen.",
            "Dieses Gespräch hat keinen Projektordner, daher kann nichts davon einem Projekt zugeordnet gespeichert werden. Ihr Assistent schlägt einen Ordner vor und stellt Ihnen eine Frage: antworten Sie einfach mit ja oder nein. Mehr ist nicht nötig.",
            "Cette conversation n'a pas de dossier de projet, donc ce qui s'y dit ne peut pas être conservé dans un projet. Votre assistant propose un dossier et vous pose une question : répondez simplement oui ou non. Rien d'autre n'est nécessaire.",
            "Esta conversación no tiene carpeta de proyecto, así que lo que se diga aquí no se puede guardar en un proyecto. Tu asistente propone una carpeta y te hace una pregunta: responde sí o no. No necesitas hacer nada más.",
        ],
        Hold::CopyProof => [
            "THOR is sending proof of this week's memory check from this computer's copy of your memory to your main computer. Your assistant does one more small step. You do not need to do anything.",
            "THOR stuurt het bewijs van de wekelijkse controle van het geheugen vanaf de kopie op deze computer naar je hoofdcomputer. Je assistent doet nog één kleine stap. Je hoeft niets te doen.",
            "THOR schickt den Nachweis der wöchentlichen Gedächtnisprüfung von der Kopie auf diesem Computer an Ihren Hauptcomputer. Ihr Assistent erledigt noch einen kleinen Schritt. Sie müssen nichts tun.",
            "THOR envoie la preuve du contrôle hebdomadaire de la mémoire, depuis la copie de cet ordinateur vers votre ordinateur principal. Votre assistant fait encore une petite étape. Vous n'avez rien à faire.",
            "THOR envía la prueba de la revisión semanal de la memoria, desde la copia de este ordenador a tu ordenador principal. Tu asistente hace un pequeño paso más. No necesitas hacer nada.",
        ],
        Hold::TidyUp => [
            "THOR asks your assistant to tidy up the memory now, before it finishes its answer. This can take a few minutes. You do not need to do anything.",
            "THOR vraagt je assistent het geheugen nu op te ruimen, voordat het zijn antwoord afmaakt. Dat kan een paar minuten duren. Je hoeft niets te doen.",
            "THOR bittet Ihren Assistenten, das Gedächtnis jetzt aufzuräumen, bevor er seine Antwort beendet. Das kann ein paar Minuten dauern. Sie müssen nichts tun.",
            "THOR demande à votre assistant de ranger la mémoire maintenant, avant de terminer sa réponse. Cela peut prendre quelques minutes. Vous n'avez rien à faire.",
            "THOR pide a tu asistente que ordene la memoria ahora, antes de terminar su respuesta. Puede tardar unos minutos. No necesitas hacer nada.",
        ],
        Hold::Decision => [
            "Your assistant is saving something you just decided, so it is remembered next time. You do not need to do anything.",
            "Je assistent bewaart iets wat je net hebt besloten, zodat het de volgende keer onthouden is. Je hoeft niets te doen.",
            "Ihr Assistent speichert gerade, was Sie eben entschieden haben, damit es beim nächsten Mal bekannt ist. Sie müssen nichts tun.",
            "Votre assistant enregistre ce que vous venez de décider, pour s'en souvenir la prochaine fois. Vous n'avez rien à faire.",
            "Tu asistente está guardando lo que acabas de decidir, para recordarlo la próxima vez. No necesitas hacer nada.",
        ],
        Hold::NotesOutOfDate => [
            "THOR noticed that some saved notes may be out of date, and asks your assistant to check them before it finishes its answer. You do not need to do anything.",
            "THOR zag dat sommige bewaarde notities verouderd kunnen zijn en vraagt je assistent ze te controleren voordat het zijn antwoord afmaakt. Je hoeft niets te doen.",
            "THOR hat bemerkt, dass einige gespeicherte Notizen veraltet sein könnten, und bittet Ihren Assistenten, sie zu prüfen, bevor er seine Antwort beendet. Sie müssen nichts tun.",
            "THOR a remarqué que certaines notes enregistrées sont peut-être périmées et demande à votre assistant de les vérifier avant de terminer sa réponse. Vous n'avez rien à faire.",
            "THOR notó que algunas notas guardadas pueden estar desactualizadas y pide a tu asistente que las revise antes de terminar su respuesta. No necesitas hacer nada.",
        ],
        Hold::Crowded => [
            "Your assistant just saved a note in a place where it would probably never be shown. THOR asks it to put the note somewhere better. You do not need to do anything.",
            "Je assistent heeft net een notitie bewaard op een plek waar die waarschijnlijk nooit getoond wordt. THOR vraagt het de notitie ergens beter neer te zetten. Je hoeft niets te doen.",
            "Ihr Assistent hat gerade eine Notiz an einer Stelle gespeichert, an der sie wohl nie angezeigt würde. THOR bittet ihn, sie an einen besseren Ort zu legen. Sie müssen nichts tun.",
            "Votre assistant vient d'enregistrer une note à un endroit où elle ne serait probablement jamais montrée. THOR lui demande de la placer ailleurs. Vous n'avez rien à faire.",
            "Tu asistente acaba de guardar una nota en un lugar donde probablemente nunca se mostraría. THOR le pide que la ponga en un sitio mejor. No necesitas hacer nada.",
        ],
        Hold::TurnViolation => [
            "THOR found that one of your files now holds something your saved notes say must not be there. Your assistant checks it and tells you plainly if it needs a decision from you. Nothing else is needed from you.",
            "THOR zag dat een van je bestanden nu iets bevat wat volgens je bewaarde notities er niet in mag staan. Je assistent controleert het en zegt het duidelijk als het een beslissing van je nodig heeft. Verder hoef je niets te doen.",
            "THOR hat festgestellt, dass eine Ihrer Dateien jetzt etwas enthält, das laut Ihren gespeicherten Notizen nicht darin stehen darf. Ihr Assistent prüft das und sagt Ihnen klar, wenn er eine Entscheidung von Ihnen braucht. Mehr ist nicht nötig.",
            "THOR a constaté que l'un de vos fichiers contient maintenant quelque chose que vos notes enregistrées interdisent. Votre assistant vérifie et vous dit clairement s'il a besoin d'une décision de votre part. Rien d'autre n'est nécessaire.",
            "THOR vio que uno de tus archivos contiene ahora algo que tus notas guardadas no permiten. Tu asistente lo revisa y te dice claramente si necesita una decisión tuya. No necesitas hacer nada más.",
        ],
        Hold::ReplyLanguage => [
            "THOR asks your assistant to rewrite its last answer in the language you chose. Nothing is needed from you.",
            "THOR vraagt je assistent zijn laatste antwoord opnieuw te schrijven in de taal die je koos. Je hoeft niets te doen.",
            "THOR bittet Ihren Assistenten, seine letzte Antwort in der von Ihnen gewählten Sprache neu zu schreiben. Sie müssen nichts tun.",
            "THOR demande à votre assistant de réécrire sa dernière réponse dans la langue que vous avez choisie. Vous n'avez rien à faire.",
            "THOR pide a tu asistente que reescriba su última respuesta en el idioma que elegiste. No necesitas hacer nada.",
        ],
        Hold::NeedsYourOk => [
            "THOR paused one step of your assistant's work because it needs your OK first. Your assistant will ask you in plain words: just answer it. Nothing else is needed from you.",
            "THOR heeft één stap van je assistent gepauzeerd, omdat eerst jouw akkoord nodig is. Je assistent vraagt het je in gewone woorden: antwoord gewoon. Verder hoef je niets te doen.",
            "THOR hat einen Schritt Ihres Assistenten angehalten, weil zuerst Ihr Einverständnis nötig ist. Ihr Assistent fragt Sie in einfachen Worten: antworten Sie einfach. Mehr ist nicht nötig.",
            "THOR a mis en pause une étape de votre assistant, car votre accord est d'abord nécessaire. Votre assistant vous le demandera simplement : répondez-lui. Rien d'autre n'est nécessaire.",
            "THOR detuvo un paso de tu asistente porque primero necesita tu visto bueno. Tu asistente te lo preguntará con palabras sencillas: solo responde. No necesitas hacer nada más.",
        ],
        Hold::ActionStopped => [
            "THOR stopped one step your assistant was about to take, because it went against something saved in the memory earlier. Your assistant will try another way. You do not need to do anything.",
            "THOR hield één stap van je assistent tegen, omdat die inging tegen iets wat eerder in het geheugen was bewaard. Je assistent probeert een andere manier. Je hoeft niets te doen.",
            "THOR hat einen Schritt Ihres Assistenten gestoppt, weil er gegen etwas verstieß, das zuvor im Gedächtnis gespeichert wurde. Ihr Assistent versucht einen anderen Weg. Sie müssen nichts tun.",
            "THOR a arrêté une étape de votre assistant, car elle allait à l'encontre de quelque chose enregistré plus tôt dans la mémoire. Votre assistant essaiera autrement. Vous n'avez rien à faire.",
            "THOR detuvo un paso de tu asistente porque iba contra algo guardado antes en la memoria. Tu asistente probará otra forma. No necesitas hacer nada.",
        ],
        Hold::StoreUnwritable => [
            "THOR cannot save anything right now: its memory file or the folder it is in is read-only, or the disk is full. Nothing was saved, so THOR does not hold your assistant back. Once the file can be written again, THOR carries on by itself, and nothing else is needed from you.",
            "THOR kan nu niets bewaren: het geheugenbestand of de map waar het in staat is alleen-lezen, of de schijf is vol. Er is niets bewaard, dus THOR houdt je assistent niet tegen. Zodra het bestand weer beschreven kan worden, gaat THOR vanzelf verder.",
            "THOR kann gerade nichts speichern: Die Gedächtnisdatei oder ihr Ordner ist schreibgeschützt, oder der Datenträger ist voll. Es wurde nichts gespeichert, daher hält THOR Ihren Assistenten nicht auf. Sobald die Datei wieder beschreibbar ist, macht THOR von selbst weiter.",
            "THOR ne peut rien enregistrer pour le moment : le fichier de mémoire ou son dossier est en lecture seule, ou le disque est plein. Rien n'a été enregistré, donc THOR ne retient pas votre assistant. Dès que le fichier est de nouveau modifiable, THOR reprend de lui-même.",
            "THOR no puede guardar nada ahora: el archivo de memoria o su carpeta es de solo lectura, o el disco está lleno. No se guardó nada, así que THOR no detiene a tu asistente. En cuanto el archivo se pueda escribir de nuevo, THOR sigue por sí mismo.",
        ],
        Hold::Neutral => [
            "THOR asks your assistant to finish one small thing first. Nothing is needed from you.",
            "THOR vraagt je assistent eerst nog één kleinigheid af te maken. Je hoeft niets te doen.",
            "THOR bittet Ihren Assistenten, zuerst noch eine Kleinigkeit zu erledigen. Sie müssen nichts tun.",
            "THOR demande à votre assistant de terminer d'abord une petite chose. Vous n'avez rien à faire.",
            "THOR pide a tu asistente que termine primero una pequeña cosa. No necesitas hacer nada.",
        ],
    };
    row[at]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_own_sentence_in_every_language() {
        for hold in ALL {
            let english = line(hold, None);
            assert!(!english.is_empty());
            for code in LANGUAGES {
                let text = line(hold, Some(code));
                assert!(text.len() > 40, "{hold:?} {code}: {text}");
                if code != "en" {
                    assert_ne!(text, english, "{hold:?} {code} is still English");
                }
                for glyph in ['\u{2014}', '\u{2013}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2026}'] {
                    assert!(!text.contains(glyph), "{hold:?} {code} holds a typographic glyph: {text}");
                }
            }
            // No two kinds share a sentence in English.
            for other in ALL {
                if other != hold {
                    assert_ne!(english, line(other, None), "{hold:?} and {other:?} say the same");
                }
            }
        }
    }

    #[test]
    fn an_unknown_or_missing_language_reads_as_english() {
        assert_eq!(line(Hold::Neutral, None), line(Hold::Neutral, Some("en")));
        assert_eq!(line(Hold::Neutral, Some("tlh")), line(Hold::Neutral, Some("en")));
        assert_eq!(line(Hold::Neutral, Some("NL")), line(Hold::Neutral, Some("nl")));
    }

    #[test]
    fn english_sentences_use_none_of_thors_vocabulary_and_say_what_is_needed() {
        for hold in ALL {
            let text = line(hold, None);
            assert!(
                text.contains("You do not need to do anything") || text.contains("is needed from you") || text.contains("otherwise nothing"),
                "{hold:?}: {text}"
            );
            assert!(text.is_ascii(), "{hold:?}: {text}");
            let words: Vec<String> = text.to_lowercase().split(|c: char| !c.is_alphanumeric()).map(str::to_string).collect();
            for jargon in ["rule", "rules", "falsifier", "evaluation", "scope", "teeth", "hook", "hooks", "mcp", "collection", "store", "retract", "lookup", "pinned"] {
                assert!(!words.iter().any(|w| w == jargon), "{hold:?} uses the word {jargon:?}: {text}");
            }
        }
    }

    #[test]
    fn each_kind_of_hold_text_is_recognised() {
        let cases = [
            ("Stop", format!("{} and belongs to no project", other_project::HOLD_MARKER), Hold::NoProject),
            ("Stop", "[THOR] This memory was just installed and its owner has not been walked through setup yet".to_string(), Hold::Setup),
            ("Stop", "[THOR] order-earlier-work: one last setup step".to_string(), Hold::Order),
            ("Stop", format!("[THOR] {} during this session\n\nThe evaluation of 'x'", leftover_scripts::HOLD_MARKER), Hold::Scripts),
            ("Stop", "[THOR] file the copy-attestation".to_string(), Hold::CopyProof),
            ("Stop", "[THOR] The evaluation of 'x' is still open".to_string(), Hold::TidyUp),
            ("Stop", "[THOR] The owner may have just stated a NEW durable decision".to_string(), Hold::Decision),
            ("Stop", "[THOR] These rules could not prove themselves while work was going on".to_string(), Hold::NotesOutOfDate),
            ("Stop", "[THOR] 'x' was stored this session onto a place that is already full\n\nThe evaluation of 'y'".to_string(), Hold::Crowded),
            ("Stop", "[THOR] A rule is broken: a file now holds text that a rule forbids".to_string(), Hold::TurnViolation),
            ("Stop", "[THOR] This reply is for the owner, but it reads as another language. Rewrite it".to_string(), Hold::ReplyLanguage),
            ("Stop", "[THOR] The memory file cannot be written right now".to_string(), Hold::StoreUnwritable),
            ("Stop", "[THOR] a reply rule fired".to_string(), Hold::Neutral),
            ("PreToolUse", "[THOR] r2: this call needs the owner's explicit go".to_string(), Hold::NeedsYourOk),
            ("PreToolUse", "[THOR] r1 forbids this text".to_string(), Hold::ActionStopped),
        ];
        for (event, reason, want) in cases {
            assert_eq!(classify(event, &reason), want, "{reason}");
        }
    }

    #[test]
    fn an_evaluation_ask_riding_after_another_hold_does_not_take_its_place() {
        let reason = "[THOR] A rule is broken: a file now holds text that a rule forbids\n\n[THOR] The evaluation of 'x' is still open";
        assert_eq!(classify("Stop", reason), Hold::TurnViolation);
    }
}
