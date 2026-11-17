import { describe, expect, it } from 'vitest';
import {
	amountToInput,
	currencyDigits,
	fileName,
	formatAmount,
	formatBytes,
	formatDate,
	isIsoDate,
	pageCount,
	parseAmount,
	parseTags,
} from './format';

describe('formatAmount', () => {
	it('formats JPY without decimals', () => {
		expect(formatAmount(1200, 'JPY', 'en-US')).toBe('¥1,200');
		expect(formatAmount(1234567, 'JPY', 'en-US')).toBe('¥1,234,567');
	});

	it('formats two-decimal currencies from minor units', () => {
		expect(formatAmount(1250, 'USD', 'en-US')).toBe('$12.50');
		expect(formatAmount(99, 'EUR', 'en-US')).toBe('€0.99');
	});

	it('keeps negative amounts', () => {
		expect(formatAmount(-500, 'JPY', 'en-US')).toBe('-¥500');
	});

	it('returns an empty string without an amount', () => {
		expect(formatAmount(null, 'JPY')).toBe('');
		expect(formatAmount(undefined, null)).toBe('');
	});

	it('shows a bare number without a currency', () => {
		expect(formatAmount(1200, null, 'en-US')).toBe('1,200');
	});

	it('falls back for a code Intl rejects', () => {
		expect(formatAmount(1250, 'not-a-code', 'en-US')).toBe('12.50 not-a-code');
	});
});

describe('amount input', () => {
	it('knows the minor-unit digits', () => {
		expect(currencyDigits('JPY')).toBe(0);
		expect(currencyDigits('USD')).toBe(2);
	});

	it('round-trips through the form field', () => {
		expect(amountToInput(1200, 'JPY')).toBe('1200');
		expect(amountToInput(1250, 'USD')).toBe('12.50');
		expect(amountToInput(null, 'USD')).toBe('');
		expect(parseAmount('1200', 'JPY')).toBe(1200);
		expect(parseAmount('12.50', 'USD')).toBe(1250);
	});

	it('accepts grouping, symbols and full-width digits', () => {
		expect(parseAmount('¥1,200', 'JPY')).toBe(1200);
		expect(parseAmount('１２００円', 'JPY')).toBe(1200);
		expect(parseAmount(' $1,234.5 ', 'USD')).toBe(123450);
	});

	it('avoids float drift', () => {
		expect(parseAmount('19.99', 'USD')).toBe(1999);
		expect(parseAmount('0.07', 'EUR')).toBe(7);
	});

	it('rejects what is not a number', () => {
		expect(parseAmount('', 'JPY')).toBeNull();
		expect(parseAmount('abc', 'JPY')).toBeNull();
		expect(parseAmount('1.2.3', 'USD')).toBeNull();
	});

	it('treats the number as minor units without a currency', () => {
		expect(parseAmount('1200', null)).toBe(1200);
	});
});

describe('dates', () => {
	it('formats a document date without shifting the day', () => {
		expect(formatDate('2024-03-01', 'en-US')).toBe('Mar 1, 2024');
		expect(formatDate('2024-12-31', 'en-US')).toBe('Dec 31, 2024');
		expect(formatDate('2024-03-01', 'ja-JP')).toBe('2024年3月1日');
	});

	it('passes through what is not a date', () => {
		expect(formatDate(null)).toBe('');
		expect(formatDate('March 2024')).toBe('March 2024');
	});

	it('validates ISO dates', () => {
		expect(isIsoDate('2024-02-29')).toBe(true);
		expect(isIsoDate('2023-02-29')).toBe(false);
		expect(isIsoDate('2024-13-01')).toBe(false);
		expect(isIsoDate('2024-1-1')).toBe(false);
	});
});

describe('small helpers', () => {
	it('formats sizes', () => {
		expect(formatBytes(512)).toBe('512 B');
		expect(formatBytes(1536)).toBe('1.5 KB');
		expect(formatBytes(48_000)).toBe('47 KB');
		expect(formatBytes(3_400_000)).toBe('3.2 MB');
	});

	it('pluralises pages', () => {
		expect(pageCount(1)).toBe('1 page');
		expect(pageCount(3)).toBe('3 pages');
	});

	it('takes the file name of a path', () => {
		expect(fileName('Health/2024/2024-03-01 Clinic receipt.pdf')).toBe(
			'2024-03-01 Clinic receipt.pdf',
		);
	});

	it('parses tags from a comma list', () => {
		expect(parseTags('tax, 2024 、 医療費,tax,,')).toEqual(['tax', '2024', '医療費']);
	});
});
