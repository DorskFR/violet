import type { Fact, Sidecar } from '../src/lib/api.ts';

type Seed = {
	title: string;
	area: string;
	type: string;
	issuer: string | null;
	date: string | null;
	lang: 'ja' | 'en';
	tags: string[];
	facts: [key: string, value: string, minor?: number, currency?: string][];
	pages: string[];
	summary?: string;
};

const SEEDS: Seed[] = [
	{
		title: '東京ガス ご使用量のお知らせ 2026年10月分',
		area: 'Housing',
		type: 'bill',
		issuer: '東京ガス',
		date: '2026-10-18',
		lang: 'ja',
		tags: ['光熱費', 'gas'],
		facts: [
			['total', '4,812円', 4812, 'JPY'],
			['usage', '21 m³'],
			['due_date', '2026-11-05'],
		],
		pages: [
			'東京ガス株式会社\nガスご使用量のお知らせ\n2026年10月分\nご使用量 21 m³\nご請求金額 4,812円（うち消費税 437円）\nお支払期限 2026年11月5日\n口座振替日 11月5日',
		],
	},
	{
		title: 'TEPCO 電気ご使用量のお知らせ 2026年10月分',
		area: 'Housing',
		type: 'bill',
		issuer: '東京電力エナジーパートナー',
		date: '2026-10-22',
		lang: 'ja',
		tags: ['光熱費', 'electricity'],
		facts: [
			['total', '9,376円', 9376, 'JPY'],
			['usage', '284 kWh'],
		],
		pages: [
			'東京電力エナジーパートナー\n電気ご使用量のお知らせ\nご使用期間 9月20日～10月19日\nご使用量 284 kWh\n請求予定金額 9,376円\n燃料費調整額 -412円\n再エネ発電賦課金 989円',
		],
	},
	{
		title: '賃貸借契約書 グランメゾン目黒 302号室',
		area: 'Housing',
		type: 'contract',
		issuer: '三井不動産レジデンシャルリース',
		date: '2024-03-15',
		lang: 'ja',
		tags: ['賃貸', 'important'],
		facts: [
			['rent', '月額 186,000円', 186000, 'JPY'],
			['deposit', '372,000円', 372000, 'JPY'],
			['term', '2024-04-01 から 2026-03-31'],
		],
		pages: [
			'賃貸借契約書\n貸主 三井不動産レジデンシャルリース株式会社\n借主 （以下「乙」という）\n物件名 グランメゾン目黒 302号室\n賃料 月額 186,000円\n共益費 月額 8,000円\n敷金 372,000円',
			'第1条（契約期間）\n契約期間は2024年4月1日から2026年3月31日までの2年間とする。\n第2条（賃料）\n乙は毎月末日までに翌月分の賃料を甲の指定する口座に振り込むものとする。',
			'第8条（禁止事項）\n乙は甲の書面による承諾なく、本物件の全部または一部を転貸してはならない。\n第12条（解約）\n乙は1か月前までに書面で通知することにより本契約を解約できる。',
			'特約事項\n退去時のハウスクリーニング費用 55,000円（税込）は乙の負担とする。\n署名捺印欄',
		],
		summary: '目黒の賃貸マンションの2年契約。賃料186,000円、敷金2か月。',
	},
	{
		title: '令和7年分 給与所得の源泉徴収票',
		area: 'Tax',
		type: 'tax form',
		issuer: '株式会社ミライテック',
		date: '2025-12-25',
		lang: 'ja',
		tags: ['源泉徴収', '確定申告', '2025'],
		facts: [
			['gross_pay', '8,420,000円', 8420000, 'JPY'],
			['withheld_tax', '742,300円', 742300, 'JPY'],
			['social_insurance', '1,214,880円', 1214880, 'JPY'],
		],
		pages: [
			'令和7年分 給与所得の源泉徴収票\n支払金額 8,420,000円\n給与所得控除後の金額 6,478,000円\n所得控除の額の合計額 2,174,880円\n源泉徴収税額 742,300円\n社会保険料等の金額 1,214,880円\n支払者 株式会社ミライテック',
		],
	},
	{
		title: '給与明細 2026年10月',
		area: 'Work',
		type: 'payslip',
		issuer: '株式会社ミライテック',
		date: '2026-10-25',
		lang: 'ja',
		tags: ['給与'],
		facts: [
			['net_pay', '512,340円', 512340, 'JPY'],
			['gross_pay', '702,000円', 702000, 'JPY'],
		],
		pages: [
			'給与明細書 2026年10月分\n基本給 620,000円\n通勤手当 12,000円\n時間外手当 70,000円\n総支給額 702,000円\n健康保険 34,650円 厚生年金 59,475円 雇用保険 4,212円\n所得税 48,320円 住民税 43,003円\n差引支給額 512,340円',
		],
	},
	{
		title: '給与明細 2026年9月',
		area: 'Work',
		type: 'payslip',
		issuer: '株式会社ミライテック',
		date: '2026-09-25',
		lang: 'ja',
		tags: ['給与'],
		facts: [
			['net_pay', '487,912円', 487912, 'JPY'],
			['gross_pay', '668,000円', 668000, 'JPY'],
		],
		pages: [
			'給与明細書 2026年9月分\n基本給 620,000円\n通勤手当 12,000円\n時間外手当 36,000円\n総支給額 668,000円\n差引支給額 487,912円',
		],
	},
	{
		title: '診療明細書・領収証 目黒さくら内科クリニック',
		area: 'Health',
		type: 'receipt',
		issuer: '目黒さくら内科クリニック',
		date: '2026-11-04',
		lang: 'ja',
		tags: ['医療費', '医療費控除'],
		facts: [
			['total', '2,340円', 2340, 'JPY'],
			['points', '780点'],
		],
		pages: [
			'領収証\n目黒さくら内科クリニック\n診療日 2026年11月4日\n初・再診料 73点 検査 412点 投薬 295点\n合計 780点\n負担割合 3割\n領収金額 2,340円',
			'診療明細書\n再診料 73点×1\n血液学的検査判断料 125点×1\n生化学的検査(I) 10項目以上 103点×1\n処方箋料 68点×1',
		],
	},
	{
		title: '調剤明細書 スギ薬局 目黒店',
		area: 'Health',
		type: 'receipt',
		issuer: 'スギ薬局',
		date: '2026-11-04',
		lang: 'ja',
		tags: ['医療費', '医療費控除', '薬'],
		facts: [['total', '1,180円', 1180, 'JPY']],
		pages: [
			'調剤明細書 兼 領収証\nスギ薬局 目黒店\nロキソプロフェンNa錠60mg 14錠\nレバミピド錠100mg 14錠\n調剤技術料 45点 薬学管理料 59点 薬剤料 289点\n患者負担額 1,180円',
		],
	},
	{
		title: '健康診断結果報告書 2026年度',
		area: 'Health',
		type: 'report',
		issuer: '慶應義塾大学病院 予防医療センター',
		date: '2026-06-12',
		lang: 'ja',
		tags: ['健康診断'],
		facts: [
			['height', '176.2 cm'],
			['weight', '71.4 kg'],
			['ldl', '118 mg/dL'],
		],
		pages: [
			'健康診断結果報告書\n受診日 2026年6月12日\n身長 176.2 cm 体重 71.4 kg BMI 23.0\n血圧 118/74 mmHg\n総合判定 A（異常なし）',
			'血液検査\nAST 21 ALT 24 γ-GTP 31\nLDLコレステロール 118 mg/dL HDLコレステロール 62 mg/dL\n中性脂肪 88 mg/dL 空腹時血糖 91 mg/dL HbA1c 5.3%',
			'医師所見\n特記すべき異常を認めません。年1回の定期健診を継続してください。',
		],
	},
	{
		title: '住民税 決定通知書 令和8年度',
		area: 'Tax',
		type: 'notice',
		issuer: '目黒区役所',
		date: '2026-06-01',
		lang: 'ja',
		tags: ['住民税', '2026'],
		facts: [
			['total', '516,000円', 516000, 'JPY'],
			['monthly', '43,000円', 43000, 'JPY'],
		],
		pages: [
			'令和8年度 特別区民税・都民税 税額決定通知書\n目黒区長\n総所得金額 6,478,000円\n所得控除合計 2,174,880円\n年税額 516,000円\n6月分 43,000円 7月分以降 43,000円',
			'（裏面）この通知書についての問い合わせ先 目黒区役所 税務課 課税第一係',
		],
	},
	{
		title: 'ふるさと納税 寄附金受領証明書 北海道紋別市',
		area: 'Tax',
		type: 'certificate',
		issuer: '北海道紋別市',
		date: '2026-09-30',
		lang: 'ja',
		tags: ['ふるさと納税', '確定申告', '2026'],
		facts: [['total', '30,000円', 30000, 'JPY']],
		pages: [
			'寄附金受領証明書\n北海道紋別市長\n寄附金額 金 30,000円\n受領年月日 令和8年9月30日\n上記の金額を地方自治法の規定に基づく寄附金として受領しました。',
		],
	},
	{
		title: '自動車保険証券 ソニー損保',
		area: 'Insurance',
		type: 'policy',
		issuer: 'ソニー損害保険',
		date: '2026-04-01',
		lang: 'ja',
		tags: ['自動車', '保険', 'important'],
		facts: [
			['premium', '年額 48,720円', 48720, 'JPY'],
			['policy_no', '5P-2026-0448213'],
			['term', '2026-04-20 から 2027-04-20'],
		],
		pages: [
			'自動車保険証券\nソニー損害保険株式会社\n証券番号 5P-2026-0448213\n保険期間 2026年4月20日 午後4時から 2027年4月20日 午後4時まで\n年間保険料 48,720円',
			'補償内容\n対人賠償 無制限 対物賠償 無制限\n人身傷害 5,000万円 車両保険 一般型 免責 5-10万円\nノンフリート等級 14等級',
		],
	},
	{
		title: '国民年金保険料 控除証明書 令和7年分',
		area: 'Tax',
		type: 'certificate',
		issuer: '日本年金機構',
		date: '2025-11-01',
		lang: 'ja',
		tags: ['年金', '確定申告', '2025'],
		facts: [['total', '203,760円', 203760, 'JPY']],
		pages: [
			'社会保険料（国民年金保険料）控除証明書\n日本年金機構\n令和7年中の納付済保険料額 203,760円\n年末調整・確定申告まで大切に保管してください。',
		],
	},
	{
		title: 'ヨドバシカメラ 領収書 MacBook Air',
		area: 'Purchases',
		type: 'receipt',
		issuer: 'ヨドバシカメラ',
		date: '2026-08-09',
		lang: 'ja',
		tags: ['家電', '保証'],
		facts: [
			['total', '198,800円', 198800, 'JPY'],
			['tax', '18,072円', 18072, 'JPY'],
		],
		pages: [
			'領収書\nヨドバシカメラ マルチメディアAkiba\nMacBook Air 13インチ M4 16GB/512GB ミッドナイト 1点 198,800円\n（内消費税等 18,072円）\nゴールドポイント 1,988ポイント\nメーカー保証 1年',
		],
	},
	{
		title: 'ニトリ 配送伝票・保証書 ダイニングテーブル',
		area: 'Purchases',
		type: 'warranty',
		issuer: 'ニトリ',
		date: '2025-05-17',
		lang: 'ja',
		tags: ['家具', '保証'],
		facts: [['total', '64,900円', 64900, 'JPY']],
		pages: [
			'お買い上げ明細 兼 保証書\n株式会社ニトリ\nダイニングテーブル 150cm ナチュラル 64,900円\n配送日 2025年5月24日\n品質保証 5年',
		],
	},
	{
		title: 'JR東日本 新幹線領収書 東京→京都',
		area: 'Travel',
		type: 'receipt',
		issuer: 'JR東日本',
		date: '2026-10-03',
		lang: 'ja',
		tags: ['出張', '経費'],
		facts: [['total', '14,170円', 14170, 'JPY']],
		pages: [
			'領収書\n東日本旅客鉄道株式会社 東京駅\n乗車券・新幹線特急券 東京 → 京都 のぞみ 普通車指定席\n金額 14,170円\n但し 旅客運賃として',
		],
	},
	{
		title: 'ANA eチケットお客様控 羽田 → 新千歳',
		area: 'Travel',
		type: 'ticket',
		issuer: 'ANA',
		date: '2026-07-18',
		lang: 'ja',
		tags: ['旅行', '北海道'],
		facts: [
			['total', '38,420円', 38420, 'JPY'],
			['booking', 'ANA 0412 85G2K7'],
		],
		pages: [
			'eチケットお客様控\n全日本空輸株式会社\nNH57 東京(羽田) 08:00 → 札幌(新千歳) 09:35\n予約番号 0412 航空券番号 205-2417 885 310\n運賃額 38,420円',
			'ご搭乗に際してのご案内\n保安検査場は出発時刻の20分前までに通過してください。',
		],
	},
	{
		title: 'マイナンバーカード 交付通知書',
		area: 'Identity',
		type: 'notice',
		issuer: '目黒区役所',
		date: '2025-02-10',
		lang: 'ja',
		tags: ['マイナンバー', 'important'],
		facts: [['deadline', '2025-05-31']],
		pages: [
			'個人番号カード交付通知書 兼 照会書\n目黒区長\n交付場所 目黒区総合庁舎 本館1階 戸籍住民課\n交付期限 令和7年5月31日\n必要な持ち物 本通知書、本人確認書類、通知カード',
		],
	},
	{
		title: '在留カード 更新許可通知',
		area: 'Identity',
		type: 'notice',
		issuer: '東京出入国在留管理局',
		date: '2026-02-20',
		lang: 'ja',
		tags: ['在留資格', 'important'],
		facts: [['fee', '4,000円', 4000, 'JPY']],
		pages: [
			'通知書\n東京出入国在留管理局\n在留期間更新許可申請について、下記のとおり処分しますので出頭してください。\n手数料 収入印紙 4,000円\n持参するもの 旅券、在留カード、この通知書',
		],
	},
	{
		title: 'NTTドコモ ご利用料金のご案内 2026年10月',
		area: 'Housing',
		type: 'bill',
		issuer: 'NTTドコモ',
		date: '2026-10-10',
		lang: 'ja',
		tags: ['通信費'],
		facts: [['total', '7,315円', 7315, 'JPY']],
		pages: [
			'ご利用料金のご案内\n株式会社NTTドコモ\neximo 基本料金 7,315円（税込）\nみんなドコモ割 -1,210円\nご請求額 7,315円\nお支払日 10月31日',
		],
	},
	{
		title: 'Amazon Web Services Invoice October 2026',
		area: 'Work',
		type: 'invoice',
		issuer: 'Amazon Web Services',
		date: '2026-11-03',
		lang: 'en',
		tags: ['cloud', 'expenses'],
		facts: [
			['total', '$84.17', 8417, 'USD'],
			['tax', '$7.65', 765, 'USD'],
			['invoice_no', '1987204415'],
		],
		pages: [
			'Amazon Web Services, Inc.\nInvoice Number: 1987204415\nBilling period: October 1 - October 31, 2026\nAmazon Simple Storage Service $12.40\nAmazon Elastic Compute Cloud $58.92\nAmazon Route 53 $5.20\nTax (JCT 10%) $7.65\nTOTAL AMOUNT DUE $84.17',
			'Detail of charges by service\nAsia Pacific (Tokyo)\nEC2 t4g.medium 744 Hrs $40.92\nEBS gp3 200 GB-Mo $18.00\nS3 Standard 520 GB $12.40',
		],
	},
	{
		title: 'Hetzner Online Invoice R0024117733',
		area: 'Work',
		type: 'invoice',
		issuer: 'Hetzner Online GmbH',
		date: '2026-10-05',
		lang: 'en',
		tags: ['cloud', 'expenses'],
		facts: [
			['total', '€46.41', 4641, 'EUR'],
			['invoice_no', 'R0024117733'],
		],
		pages: [
			'Hetzner Online GmbH\nInvoice no.: R0024117733\nInvoice date: 05/10/2026\nDedicated Server AX42 09/2026 €39.00\nStorage Box BX11 €3.20\nIPv4 address €1.70\nSubtotal €43.90 VAT 0% reverse charge\nAmount due: €46.41',
		],
	},
	{
		title: 'Employment Agreement — Senior Software Engineer',
		area: 'Work',
		type: 'contract',
		issuer: 'Miraitech K.K.',
		date: '2023-01-16',
		lang: 'en',
		tags: ['employment', 'important'],
		facts: [
			['salary', 'JPY 7,440,000 per year', 7440000, 'JPY'],
			['start_date', '2023-02-01'],
		],
		pages: [
			'EMPLOYMENT AGREEMENT\nThis Employment Agreement is entered into between Miraitech K.K. (the "Company") and the Employee.\nPosition: Senior Software Engineer\nCommencement date: February 1, 2023\nBase salary: JPY 7,440,000 per year, paid in twelve monthly instalments.',
			'Article 4 (Working hours)\nThe Company adopts a flextime system with core hours from 11:00 to 15:00.\nArticle 5 (Paid leave)\nThe Employee is granted 15 days of annual paid leave in the first year.',
			'Article 9 (Confidentiality)\nThe Employee shall not disclose confidential information during or after employment.\nArticle 11 (Termination)\nEither party may terminate this Agreement with 30 days written notice.',
			'Article 14 (Governing law)\nThis Agreement is governed by the laws of Japan.\nSigned in Tokyo on January 16, 2023.',
			'Appendix A — Stock options\nThe Employee is granted 2,400 options vesting over four years with a one-year cliff.',
		],
		summary: 'Full-time employment contract with Miraitech, flextime, 30 days notice.',
	},
	{
		title: 'Wise Statement EUR September 2026',
		area: 'Finance',
		type: 'statement',
		issuer: 'Wise',
		date: '2026-10-01',
		lang: 'en',
		tags: ['bank', 'EUR'],
		facts: [
			['closing_balance', '€2,318.44', 231844, 'EUR'],
			['account', 'BE68 9670 1234 5678'],
		],
		pages: [
			'Wise Europe SA\nEUR statement 1 September 2026 - 30 September 2026\nIBAN BE68 9670 1234 5678\nOpening balance 1,904.12 EUR\nMoney in 1,200.00 EUR Money out 785.68 EUR\nClosing balance 2,318.44 EUR',
			'Transactions\n03 Sep Card transaction Hetzner Online -46.41\n11 Sep Received from ACME SARL +1,200.00\n19 Sep Converted to JPY -700.00\n27 Sep Card transaction Deutsche Bahn -39.27',
		],
	},
	{
		title: '楽天銀行 取引明細 2026年10月',
		area: 'Finance',
		type: 'statement',
		issuer: '楽天銀行',
		date: '2026-11-01',
		lang: 'ja',
		tags: ['bank'],
		facts: [['closing_balance', '1,482,119円', 1482119, 'JPY']],
		pages: [
			'楽天銀行 取引明細書\n対象期間 2026年10月1日～10月31日\n10/05 口座振替 トウキヨウガス -4,812\n10/25 給与 カ）ミライテツク +512,340\n10/27 振込 ミツイフドウサン -194,000\n月末残高 1,482,119円',
			'10/28 口座振替 NTTドコモ -7,315\n10/30 ATM出金 -30,000\n10/31 利息 +12',
		],
	},
	{
		title: 'SBI証券 特定口座年間取引報告書 2025年',
		area: 'Finance',
		type: 'report',
		issuer: 'SBI証券',
		date: '2026-01-15',
		lang: 'ja',
		tags: ['証券', '確定申告', '2025'],
		facts: [
			['gain', '譲渡益 412,880円', 412880, 'JPY'],
			['withheld_tax', '83,877円', 83877, 'JPY'],
			['dividends', '96,240円', 96240, 'JPY'],
		],
		pages: [
			'令和7年分 特定口座年間取引報告書\n株式会社SBI証券\n譲渡の対価の額 3,218,400円 取得費及び譲渡に要した費用の額等 2,805,520円\n差引金額（譲渡所得等の金額） 412,880円\n源泉徴収税額 83,877円',
			'配当等の額及び源泉徴収税額等\n特定上場株式等の配当等 96,240円\n源泉徴収税額（所得税） 14,739円 配当割額（住民税） 4,812円',
		],
	},
	{
		title: 'Apple Store Receipt — AirPods Pro',
		area: 'Purchases',
		type: 'receipt',
		issuer: 'Apple',
		date: '2026-03-21',
		lang: 'en',
		tags: ['electronics', 'warranty'],
		facts: [
			['total', '¥39,800', 39800, 'JPY'],
			['order_no', 'W1284471903'],
		],
		pages: [
			'Apple Store, Omotesando\nReceipt W1284471903\nAirPods Pro (3rd generation) ¥39,800\nConsumption tax (10%) included ¥3,618\nPaid with Visa ending 4421\nReturn by April 4, 2026',
		],
	},
	{
		title: 'Booking.com Confirmation — Hotel Granvia Kyoto',
		area: 'Travel',
		type: 'booking',
		issuer: 'Booking.com',
		date: '2026-10-02',
		lang: 'en',
		tags: ['hotel', 'expenses'],
		facts: [
			['total', '¥42,600', 42600, 'JPY'],
			['confirmation', '4418.220.917'],
		],
		pages: [
			'Booking confirmation\nConfirmation number: 4418.220.917\nHotel Granvia Kyoto\nCheck-in Saturday, October 3, 2026 Check-out Monday, October 5, 2026\n1 Superior Double Room, 2 nights\nTotal price ¥42,600 including taxes',
			'Cancellation policy\nFree cancellation until October 1, 2026 18:00 (Kyoto time).\nBreakfast is not included in the room rate.',
		],
	},
	{
		title: '車検証・自動車税納税証明書 2026',
		area: 'Vehicle',
		type: 'certificate',
		issuer: '東京都主税局',
		date: '2026-05-28',
		lang: 'ja',
		tags: ['自動車', '税'],
		facts: [['total', '34,500円', 34500, 'JPY']],
		pages: [
			'自動車税種別割 納税証明書（継続検査用）\n東京都主税局\n登録番号 品川 300 あ 12-34\n税額 34,500円\n納付日 令和8年5月28日',
			'自動車検査証\n車名 マツダ 型式 6BA-KF5P\n有効期間の満了する日 令和10年6月14日',
		],
	},
	{
		title: 'Letter from HMRC — Self Assessment statement',
		area: 'Tax',
		type: 'letter',
		issuer: 'HM Revenue & Customs',
		date: null,
		lang: 'en',
		tags: ['UK', 'needs review'],
		facts: [['balance', '£0.00', 0, 'GBP']],
		pages: [
			'HM Revenue & Customs\nSelf Assessment Statement\nYou have nothing to pay. Your account balance is £0.00.\nKeep this statement for your records. <b>Unique Taxpayer Reference</b> 51 847 22913 & co.',
		],
	},
];

export type MockDocument = {
	path: string;
	size: number;
	sidecar: Sidecar;
	/** Per page, parallel to `sidecar.pages`: what the fake scan looks like. */
	scans: { seed: number; rotation: number }[];
};

let counter = 0;

export function uuid(): string {
	counter += 1;
	return `0193a7c2-5b1e-7000-8000-${counter.toString(16).padStart(12, '0')}`;
}

export function sha(seed: number): string {
	let s = (seed * 2654435761) >>> 0;
	let out = '';
	while (out.length < 64) {
		s = (Math.imul(s ^ (s >>> 15), 2246822507) + 0x9e3779b9) >>> 0;
		out += s.toString(16).padStart(8, '0');
	}
	return out.slice(0, 64);
}

export function libraryPath(sidecar: Pick<Sidecar, 'area' | 'date' | 'title'>): string {
	const year = sidecar.date?.slice(0, 4) ?? 'undated';
	const stem = `${sidecar.date ? `${sidecar.date} ` : ''}${sidecar.title}`.replace(/[/\\:]/g, ' ');
	return `${sidecar.area}/${year}/${stem}.pdf`;
}

export function buildLibrary(now: Date): MockDocument[] {
	return SEEDS.map((seed, i) => {
		const created = new Date(now.getTime() - (i * 9 + 2) * 3_600_000).toISOString();
		const facts: Fact[] = seed.facts.map(([key, value, minor, currency]) => ({
			key,
			value,
			amount_minor: minor ?? null,
			currency: currency ?? null,
		}));
		const sidecar: Sidecar = {
			version: 1,
			id: uuid(),
			title: seed.title,
			area: seed.area,
			doc_type: seed.type,
			issuer: seed.issuer,
			date: seed.date,
			language: seed.lang,
			summary: seed.summary ?? null,
			tags: seed.tags,
			facts,
			pages: seed.pages.map((text, p) => ({
				number: p + 1,
				text,
				source: i % 4 === 0 ? 'layer' : 'ocr',
			})),
			sources: [
				{
					sha256: sha(i + 1000),
					original_name: `scan_${String(2041 + i).padStart(4, '0')}.pdf`,
					pages: [],
				},
			],
			sha256: sha(i + 1),
			confidence: seed.date ? 0.86 + ((i * 7) % 13) / 100 : 0.41,
			created_at: created,
			updated_at: created,
		};
		return {
			path: libraryPath(sidecar),
			size: 48_000 + seed.pages.length * 131_000 + i * 977,
			sidecar,
			scans: seed.pages.map((_, p) => ({ seed: (i + 1) * 100 + p, rotation: 0 })),
		};
	});
}
