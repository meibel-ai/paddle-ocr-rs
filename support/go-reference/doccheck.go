package ui

// Verifica manomissioni ("--shell-checkdoc"): integra la libreria chk_defaced
// (© Dario Finardi, integrata su autorizzazione dell'autore) tramite
// EditoDocCheck.dll per rilevare documenti "defaced" — font manomessi in cui
// il testo estratto diverge da quello mostrato, e testo invisibile/occultato
// (vettore di prompt-injection e clausole nascoste). Questo file contiene il
// modello del Report JSON e il requester che lo presenta.

import (
	"encoding/json"
	"fmt"
	"image/color"
	"path/filepath"
	"sort"
	"strings"

	"fyne.io/fyne/v2"
	"fyne.io/fyne/v2/app"
	"fyne.io/fyne/v2/canvas"
	"fyne.io/fyne/v2/container"
	"fyne.io/fyne/v2/layout"
	"fyne.io/fyne/v2/theme"
	"fyne.io/fyne/v2/widget"

	"github.com/jugaad/pdfafix/osutil"
	"github.com/jugaad/pdfafix/util"
)

// ── Modello del Report di chk_defaced (speculare al JSON serde) ──

type docCheckAssessment struct {
	OK          bool    `json:"ok"`
	Defaced     bool    `json:"defaced"`
	HiddenText  bool    `json:"hidden_text"`
	MaxSeverity *string `json:"max_severity"`
}

type docCheckMetadata struct {
	Title          *string `json:"title"`
	Author         *string `json:"author"`
	LastModifiedBy *string `json:"last_modified_by"`
	CreatorTool    *string `json:"creator_tool"`
	Producer       *string `json:"producer"`
	Company        *string `json:"company"`
	Created        *string `json:"created"`
	Modified       *string `json:"modified"`
	Revision       *string `json:"revision"`
}

type docCheckFinding struct {
	Rule     string `json:"rule"`
	Severity string `json:"severity"` // Info | Low | Medium | High | Critical
	Category string `json:"category"`
	Message  string `json:"message"`
	Location string `json:"location"`
}

type docCheckPhrase struct {
	Extracted string  `json:"extracted"`
	Presumed  string  `json:"presumed"`
	Page      *uint32 `json:"page"`
}

type docCheckReport struct {
	File          string              `json:"file"`
	Format        string              `json:"format"`
	FontsExamined int                 `json:"fonts_examined"`
	Metadata      *docCheckMetadata   `json:"metadata"`
	Assessment    *docCheckAssessment `json:"assessment"`
	Findings      []docCheckFinding   `json:"findings"`
	Phrases       []docCheckPhrase    `json:"phrases"`
	Error         string              `json:"error"` // valorizzato solo nel JSON d'errore della DLL
}

// ── Ingresso dal menu contestuale ──

// RunDocCheckShell gestisce "--shell-checkdoc <file>": piccola finestra di
// attesa, scansione in background, poi il report strutturato nella stessa
// finestra. Processo autonomo come le altre azioni shell.
func RunDocCheckShell(path string) {
	a := app.NewWithID(util.AppID)
	a.SetIcon(fyne.NewStaticResource("app_icon.png", AppIcon))
	w := a.NewWindow(tr(a, "doccheck_title"))
	w.SetIcon(fyne.NewStaticResource("app_icon.png", AppIcon))

	busy := widget.NewLabel(tr(a, "doccheck_running", filepath.Base(path)))
	busy.Wrapping = fyne.TextWrapWord
	bar := widget.NewProgressBarInfinite()
	w.SetContent(container.NewPadded(container.NewVBox(busy, bar)))
	ts := theme.TextSize()
	w.Resize(fyne.NewSize(ts*28, ts*9))
	w.CenterOnScreen()

	go func() {
		var rep docCheckReport
		var scanErr string
		if !osutil.DocCheckAvailable() {
			scanErr = tr(a, "doccheck_unavailable")
		} else if out, err := osutil.DocCheckScan(path); err != nil {
			scanErr = err.Error()
		} else if err := json.Unmarshal([]byte(out), &rep); err != nil {
			scanErr = err.Error()
		} else if rep.Error != "" {
			scanErr = rep.Error
		}
		fyne.Do(func() {
			bar.Stop()
			showDocCheckReport(a, w, path, &rep, scanErr)
		})
	}()

	w.ShowAndRun()
}

// ── Presentazione ──

// severityBadge ritorna l'etichetta di severità colorata, con tinte leggibili
// su entrambi i temi. Info/Low usano un grigio smorzato ma ben distinguibile
// dallo sfondo (l'Importance "Low" di Fyne risultava quasi invisibile).
func severityBadge(a fyne.App, sev string) *canvas.Text {
	l := canvas.NewText(tr(a, "sev_"+strings.ToLower(sev)), severityColor(a, sev))
	l.TextStyle = fyne.TextStyle{Bold: true}
	l.TextSize = theme.TextSize()
	return l
}

// findingGroup: rilevazioni della stessa regola aggregate in una sola voce.
type findingGroup struct {
	rule, severity, category, message, location string
	count                                       int
}

// groupFindings raggruppa le rilevazioni per regola (mantenendo severità,
// messaggio e posizione della prima occorrenza) e le ordina per severità
// decrescente. Riduce a poche voci gli elenchi con decine di rilevazioni
// ripetute, evitando che la UI generi centinaia di widget e si blocchi.
func groupFindings(findings []docCheckFinding) []*findingGroup {
	var groups []*findingGroup
	idx := map[string]*findingGroup{}
	for _, f := range findings {
		g := idx[f.Rule]
		if g == nil {
			g = &findingGroup{rule: f.Rule, severity: f.Severity, category: f.Category,
				message: f.Message, location: f.Location}
			idx[f.Rule] = g
			groups = append(groups, g)
		}
		g.count++
	}
	sort.SliceStable(groups, func(i, j int) bool {
		return sevRank(groups[i].severity) > sevRank(groups[j].severity)
	})
	return groups
}

// sevRank ordina le severità di chk_defaced (Info < Low < Medium < High < Critical).
func sevRank(s string) int {
	switch s {
	case "Critical":
		return 4
	case "High":
		return 3
	case "Medium":
		return 2
	case "Low":
		return 1
	default: // Info
		return 0
	}
}

// severityColor: rosso mattone/ambra/grigio smorzato, con varianti per il tema
// scuro. Coerente con docCheckHeadColor.
func severityColor(a fyne.App, sev string) color.Color {
	dark := a.Settings().ThemeVariant() == theme.VariantDark
	switch sev {
	case "High", "Critical":
		return docCheckHeadColor(a, "err")
	case "Medium":
		return docCheckHeadColor(a, "warn")
	default: // Info, Low — grigio leggibile, non il grigio quasi-invisibile di Fyne
		if dark {
			return color.NRGBA{R: 0x9E, G: 0xA4, B: 0xAC, A: 0xFF}
		}
		return color.NRGBA{R: 0x5F, G: 0x66, B: 0x6E, A: 0xFF}
	}
}

// metaRow aggiunge una riga label→valore alla griglia della provenienza (solo
// se il campo è presente nel documento).
func metaRow(a fyne.App, grid *fyne.Container, key string, val *string) {
	if val == nil || *val == "" {
		return
	}
	k := widget.NewLabelWithStyle(tr(a, key), fyne.TextAlignTrailing, fyne.TextStyle{Bold: true})
	v := widget.NewLabel(*val)
	v.Wrapping = fyne.TextWrapWord
	grid.Add(k)
	grid.Add(v)
}

// docCheckHeadColors: colori dell'esito, scelti sobri (il verde "success" del
// tema Fyne è troppo acceso per un'intestazione). Variante leggibile per tema
// chiaro e scuro.
func docCheckHeadColor(a fyne.App, kind string) color.Color {
	dark := a.Settings().ThemeVariant() == theme.VariantDark
	switch kind {
	case "ok":
		if dark {
			return color.NRGBA{R: 0x81, G: 0xC7, B: 0x84, A: 0xFF} // verde salvia chiaro
		}
		return color.NRGBA{R: 0x2E, G: 0x7D, B: 0x32, A: 0xFF} // verde bosco
	case "warn":
		if dark {
			return color.NRGBA{R: 0xFF, G: 0xB7, B: 0x4D, A: 0xFF}
		}
		return color.NRGBA{R: 0xB2, G: 0x6A, B: 0x00, A: 0xFF} // ambra scura
	default: // "err"
		if dark {
			return color.NRGBA{R: 0xE5, G: 0x73, B: 0x73, A: 0xFF}
		}
		return color.NRGBA{R: 0xB0, G: 0x2A, B: 0x2A, A: 0xFF} // rosso mattone
	}
}

// showDocCheckReport sostituisce il contenuto della finestra con il report
// strutturato: esito in evidenza, provenienza, elenco rilevazioni e frasi
// interessate, in stile coerente con il resto dell'applicazione (finestra
// dedicata, dimensioni dinamiche in em, pulsante Chiudi accent).
func showDocCheckReport(a fyne.App, w fyne.Window, path string, rep *docCheckReport, scanErr string) {
	ts := theme.TextSize()

	// Il verdetto "testo nascosto" può essere un FALSO POSITIVO quando l'unico
	// segnale d'occultamento è il render mode invisibile (Tr 3): è anche il
	// normale livello di testo OCR ricercabile dei PDF scansionati — che la
	// nostra stessa app genera rasterizzando. Se è l'unica causa, attenuiamo
	// l'allarme e lo segnaliamo esplicitamente.
	hiddenDrivers, nonRenderModeHidden := 0, 0
	for _, f := range rep.Findings {
		if f.Category == "HiddenContent" && sevRank(f.Severity) >= sevRank("Medium") {
			hiddenDrivers++
			if f.Rule != "PDF.INVISIBLE_RENDER_MODE" {
				nonRenderModeHidden++
			}
		}
	}
	renderModeOnly := hiddenDrivers > 0 && nonRenderModeHidden == 0

	// ── Esito in evidenza ──
	var headIcon fyne.Resource
	var headText, headKind string
	sub := widget.NewLabel("")
	sub.Wrapping = fyne.TextWrapWord
	switch {
	case scanErr != "":
		headIcon, headText, headKind = theme.ErrorIcon(), tr(a, "doccheck_error"), "err"
		sub.SetText(scanErr)
	case rep.Assessment != nil && rep.Assessment.Defaced:
		headIcon, headText, headKind = theme.ErrorIcon(), tr(a, "doccheck_defaced"), "err"
		sub.SetText(tr(a, "doccheck_defaced_sub"))
	case rep.Assessment != nil && rep.Assessment.HiddenText && renderModeOnly:
		// Probabile layer OCR di un PDF scansionato: attenzione, ma con nota di
		// possibile falso positivo (icona informativa, testo meno allarmante).
		headIcon, headText, headKind = theme.InfoIcon(), tr(a, "doccheck_hidden_maybe"), "warn"
		sub.SetText(tr(a, "doccheck_hidden_maybe_sub"))
	case rep.Assessment != nil && rep.Assessment.HiddenText:
		headIcon, headText, headKind = theme.WarningIcon(), tr(a, "doccheck_hidden"), "warn"
		sub.SetText(tr(a, "doccheck_hidden_sub"))
	case rep.Assessment != nil && !rep.Assessment.OK:
		headIcon, headText, headKind = theme.WarningIcon(), tr(a, "doccheck_suspect"), "warn"
		sub.SetText(tr(a, "doccheck_suspect_sub"))
	default:
		headIcon, headText, headKind = theme.ConfirmIcon(), tr(a, "doccheck_ok"), "ok"
		if len(rep.Findings) > 0 {
			sub.SetText(tr(a, "doccheck_ok_notes", len(rep.Findings)))
		} else {
			sub.SetText(tr(a, "doccheck_ok_sub"))
		}
	}
	head := canvas.NewText(headText, docCheckHeadColor(a, headKind))
	head.TextStyle = fyne.TextStyle{Bold: true}
	head.TextSize = ts * 1.2
	fileLine := widget.NewLabelWithStyle(filepath.Base(path), fyne.TextAlignLeading, fyne.TextStyle{Italic: true})
	fileLine.Wrapping = fyne.TextWrapWord
	if rep.Format != "" {
		fileLine.SetText(fmt.Sprintf("%s   ·   %s   ·   %s", filepath.Base(path),
			strings.ToUpper(rep.Format), tr(a, "doccheck_fonts", rep.FontsExamined)))
	}
	header := container.NewBorder(nil, nil,
		container.NewVBox(widget.NewIcon(headIcon)), nil,
		container.NewVBox(head, sub, fileLine))

	body := container.NewVBox()
	metaRows := 0

	// ── Provenienza (metadata del contenitore) ──
	if m := rep.Metadata; m != nil {
		grid := container.New(layout.NewFormLayout())
		metaRow(a, grid, "doccheck_meta_title", m.Title)
		metaRow(a, grid, "doccheck_meta_author", m.Author)
		metaRow(a, grid, "doccheck_meta_lastmod", m.LastModifiedBy)
		metaRow(a, grid, "doccheck_meta_tool", m.CreatorTool)
		metaRow(a, grid, "doccheck_meta_producer", m.Producer)
		metaRow(a, grid, "doccheck_meta_company", m.Company)
		metaRow(a, grid, "doccheck_meta_created", m.Created)
		metaRow(a, grid, "doccheck_meta_modified", m.Modified)
		metaRow(a, grid, "doccheck_meta_revision", m.Revision)
		if len(grid.Objects) > 0 {
			metaRows = len(grid.Objects) / 2
			body.Add(widget.NewLabelWithStyle(tr(a, "doccheck_meta"), fyne.TextAlignLeading, fyne.TextStyle{Bold: true}))
			body.Add(grid)
		}
	}

	// ── Rilevazioni (raggruppate per regola) ──
	// Molti documenti producono decine di voci della STESSA regola (es. un
	// /Differences per font): renderizzarle una per una crea centinaia di widget
	// e blocca la UI. Le raggruppiamo per regola — una riga con il conteggio —
	// così il numero di widget resta limitato al numero di regole distinte.
	groups := groupFindings(rep.Findings)
	if len(groups) > 0 {
		body.Add(widget.NewSeparator())
		body.Add(widget.NewLabelWithStyle(tr(a, "doccheck_findings", len(rep.Findings)),
			fyne.TextAlignLeading, fyne.TextStyle{Bold: true}))
		for _, g := range groups {
			label := g.rule
			if g.count > 1 {
				label = fmt.Sprintf("%s  (%d×)", g.rule, g.count)
			}
			msg := widget.NewLabel(g.message)
			msg.Wrapping = fyne.TextWrapWord
			rule := widget.NewLabelWithStyle(label, fyne.TextAlignLeading, fyne.TextStyle{Monospace: true})
			row := container.NewVBox(
				container.NewHBox(severityBadge(a, g.severity), rule),
				msg,
			)
			if g.count == 1 && g.location != "" {
				loc := widget.NewLabelWithStyle(g.location, fyne.TextAlignLeading, fyne.TextStyle{Italic: true})
				loc.Wrapping = fyne.TextWrapWord
				row.Add(loc)
			}
			// Nota di possibile falso positivo per il render mode invisibile:
			// coincide con il livello di testo OCR ricercabile delle scansioni.
			if g.rule == "PDF.INVISIBLE_RENDER_MODE" {
				fp := widget.NewLabelWithStyle("⚠ "+tr(a, "doccheck_fp_ocr"),
					fyne.TextAlignLeading, fyne.TextStyle{Italic: true})
				fp.Wrapping = fyne.TextWrapWord
				row.Add(fp)
			}
			body.Add(row)
		}
	} else if scanErr == "" {
		body.Add(widget.NewSeparator())
		none := widget.NewLabel(tr(a, "doccheck_none"))
		none.Wrapping = fyne.TextWrapWord
		body.Add(none)
	}

	// ── Frasi interessate (estratto vs presunto reso) ──
	if len(rep.Phrases) > 0 {
		body.Add(widget.NewSeparator())
		body.Add(widget.NewLabelWithStyle(tr(a, "doccheck_phrases"),
			fyne.TextAlignLeading, fyne.TextStyle{Bold: true}))
		const maxShown = 20
		for i, p := range rep.Phrases {
			if i == maxShown {
				body.Add(widget.NewLabelWithStyle(tr(a, "doccheck_phrases_more", len(rep.Phrases)-maxShown),
					fyne.TextAlignLeading, fyne.TextStyle{Italic: true}))
				break
			}
			ext := widget.NewLabel(tr(a, "doccheck_extracted") + ": " + p.Extracted)
			ext.Wrapping = fyne.TextWrapWord
			pre := widget.NewLabel(tr(a, "doccheck_presumed") + ": " + p.Presumed)
			pre.Wrapping = fyne.TextWrapWord
			pre.Importance = widget.WarningImportance
			row := container.NewVBox(ext, pre)
			if p.Page != nil {
				row.Add(widget.NewLabelWithStyle(tr(a, "doccheck_page", *p.Page),
					fyne.TextAlignLeading, fyne.TextStyle{Italic: true}))
			}
			body.Add(row)
			body.Add(widget.NewSeparator())
		}
	}

	closeBtn := widget.NewButton(tr(a, "close_btn"), func() { a.Quit() })
	closeBtn.Importance = widget.HighImportance
	bar := container.NewHBox(layout.NewSpacer(), closeBtn)

	// Invito all'acquisto in-app: solo nel canale Store e solo se non ancora
	// sbloccata (altrove newStoreUpsell ritorna nil e il piede resta invariato).
	bottom := container.NewVBox(container.NewPadded(bar))
	upsellEm := float32(0)
	if up := newStoreUpsell(a, w, nil); up != nil {
		bottom = container.NewVBox(container.NewPadded(up), container.NewPadded(bar))
		upsellEm = 6.0
	}

	content := container.NewBorder(
		container.NewVBox(container.NewPadded(header), widget.NewSeparator()),
		bottom, nil, nil,
		container.NewVScroll(container.NewPadded(body)),
	)
	w.SetContent(content)

	// Dimensioni dinamiche in em, mai pixel fissi. Obiettivo: se il contenuto è
	// breve la finestra lo mostra INTERO, senza scrollbar; solo i report lunghi
	// scorrono (oltre il tetto adatto a uno schermo tipico). La stima parte
	// dall'ingombro fisso (intestazione + pulsante ~14 em) e somma il corpo con
	// margini prudenziali per gli a-capo (valori di provenienza e messaggi delle
	// rilevazioni spesso vanno su più righe).
	winW := ts * 36 // un filo più larga: i valori di provenienza vanno meno a capo
	const chromeEm = 14.0
	bodyEm := float32(0)
	if metaRows > 0 {
		bodyEm += 1.6 + float32(metaRows)*2.0 // titolo "Provenienza" + righe (con margine a-capo)
	}
	if len(groups) > 0 {
		bodyEm += 1.6 + float32(len(groups))*5.5 // titolo "Rilevazioni" + una voce per regola
	} else if scanErr == "" {
		bodyEm += 1.6 // riga "nessuna anomalia"
	}
	if len(rep.Phrases) > 0 {
		bodyEm += 1.6 + float32(len(rep.Phrases))*3.6
	}
	winH := (ts * (chromeEm + upsellEm + bodyEm)) * 1.05
	if maxH := ts * 50; winH > maxH { // ~850px: entra su schermi da 900px+ in su
		winH = maxH
	}
	if minH := ts * 26; winH < minH {
		winH = minH
	}
	w.Resize(fyne.NewSize(winW, winH))
	w.CenterOnScreen()
}
